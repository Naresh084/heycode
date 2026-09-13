//! PL05 marketplace pin, provenance and substitution product contracts.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;

use heycode_extensions::{
    ApiVersion, Architecture, CatalogDigest, InstalledPlugin, MARKETPLACE_CATALOG_SCHEMA_VERSION,
    ManifestValidator, MarketplaceCatalog, MarketplaceError, MarketplaceId, MarketplaceSource,
    MarketplaceSourceKind, OperatingSystem, PackageOrigin, PackageProvenance, PlatformTarget,
    PluginId, PluginInstallCache, PluginSourceKind, PluginVersion, SignatureState, Substitution,
};

const CANONICAL_SIGNATURE: &str =
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

fn validator() -> ManifestValidator {
    ManifestValidator::new(
        ApiVersion::new(1).unwrap(),
        PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
    )
}

fn id(value: &str) -> PluginId {
    PluginId::new(value).unwrap()
}

fn version(value: &str) -> PluginVersion {
    PluginVersion::parse(value).unwrap()
}

fn marketplace(value: &str) -> MarketplaceId {
    MarketplaceId::new(value).unwrap()
}

/// A package manifest whose `[source]` block is under the test's control, so a
/// package can claim any origin it likes.
fn manifest(plugin: &str, package_version: &str, source_kind: &str, locator: &str) -> String {
    format!(
        r#"schema_version = 1
id = "{plugin}"
name = "Marketplace fixture"
version = "{package_version}"
description = "A deterministic marketplace fixture."
license = "MIT"
default_enabled = false
requested_permissions = []
platforms = [{{ os = "macos", architecture = "aarch64" }}]
dependencies = []
conflicts = []
contributions = [{{ kind = "skill", id = "review", path = "skills/review/SKILL.md", exposure = {{ mode = "namespaced" }} }}]

[api]
minimum = 1
maximum = 1

[source]
kind = "{source_kind}"
locator = "{locator}"
revision = "{package_version}"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#,
    )
}

fn write_package(root: &Path, plugin: &str, package_version: &str, body: &str) {
    write_claiming_package(
        root,
        plugin,
        package_version,
        body,
        "local",
        "fixtures/package",
    );
}

fn write_claiming_package(
    root: &Path,
    plugin: &str,
    package_version: &str,
    body: &str,
    source_kind: &str,
    locator: &str,
) {
    std::fs::create_dir_all(root.join(".heycode-plugin")).unwrap();
    std::fs::create_dir_all(root.join("skills/review")).unwrap();
    std::fs::write(
        root.join(".heycode-plugin/plugin.toml"),
        manifest(plugin, package_version, source_kind, locator),
    )
    .unwrap();
    std::fs::write(root.join("skills/review/SKILL.md"), body).unwrap();
}

/// One package installed into its own cache root, so two different byte
/// sequences can exist under the same id and version — which is exactly the
/// state an operator's machine is in after a source substitutes bytes.
fn install(
    temp: &tempfile::TempDir,
    label: &str,
    plugin: &str,
    package_version: &str,
    body: &str,
) -> (PluginInstallCache, InstalledPlugin) {
    let source = temp.path().join(format!("{label}-source"));
    write_package(&source, plugin, package_version, body);
    let cache = PluginInstallCache::open(temp.path().join(label), validator()).unwrap();
    let installed = cache.install_directory(&source).unwrap();
    (cache, installed)
}

fn https_source(name: &str, catalog: &str) -> MarketplaceSource {
    MarketplaceSource::new(
        marketplace(name),
        MarketplaceSourceKind::Https,
        "https://marketplace.example.com/catalog.toml",
        CatalogDigest::of_bytes(catalog.as_bytes()).as_str(),
    )
    .unwrap()
}

fn parse(source: &MarketplaceSource, catalog: &str) -> MarketplaceCatalog {
    MarketplaceCatalog::parse_pinned(source, catalog.as_bytes()).unwrap()
}

/// A catalog holding one row for each supplied id/version/digest triple.
fn catalog_of(name: &str, rows: &[(&str, &str, &str)]) -> String {
    let mut document = format!("schema_version = 1\nmarketplace = \"{name}\"\n");
    for (plugin, package_version, content) in rows {
        document.push_str(&format!(
            "\n[[packages]]\nid = \"{plugin}\"\nversion = \"{package_version}\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/{plugin}\"\ncontent = \"{content}\"\n",
        ));
    }
    document
}

// --- Marketplace source configuration -----------------------------------

#[test]
fn a_marketplace_source_without_a_well_formed_catalog_digest_cannot_be_configured() {
    let error = MarketplaceSource::new(
        marketplace("acme"),
        MarketplaceSourceKind::Https,
        "https://marketplace.example.com/catalog.toml",
        "sha256:not-a-digest",
    )
    .unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidField {
            field: "catalog_digest",
            reason: "must be sha256 followed by 64 lowercase hexadecimal digits",
        }
    );
}

#[test]
fn a_marketplace_source_locator_carrying_url_credentials_is_refused() {
    let error = MarketplaceSource::new(
        marketplace("acme"),
        MarketplaceSourceKind::Https,
        "https://user:secret@marketplace.example.com/catalog.toml",
        &format!("sha256:{}", "0".repeat(64)),
    )
    .unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidField {
            field: "source.locator",
            reason: "must be a bounded public locator without credentials, query strings, or traversal",
        }
    );
}

/// Which locator grammar a source is held to follows its kind. A local
/// marketplace names a portable relative path beneath a host-chosen root, so a
/// configured absolute path — or a traversal out of that root — is refused
/// before anything joins it to a path.
#[test]
fn a_local_marketplace_locator_must_be_a_portable_relative_path() {
    let digest = format!("sha256:{}", "0".repeat(64));
    MarketplaceSource::new(
        marketplace("acme"),
        MarketplaceSourceKind::LocalDirectory,
        "catalogs/acme/catalog.toml",
        &digest,
    )
    .unwrap();

    for locator in ["/etc/heycode/catalog.toml", "../../etc/catalog.toml"] {
        assert_eq!(
            MarketplaceSource::new(
                marketplace("acme"),
                MarketplaceSourceKind::LocalDirectory,
                locator,
                &digest,
            )
            .unwrap_err(),
            MarketplaceError::InvalidField {
                field: "source.locator",
                reason: "must be a bounded public locator without credentials, query strings, or traversal",
            }
        );
    }
}

// --- Catalog admission ---------------------------------------------------

#[test]
fn catalog_bytes_that_miss_the_pinned_digest_are_refused_before_the_document_is_parsed() {
    let pinned = catalog_of(
        "acme",
        &[("acme/tools", "1.0.0", &format!("sha256:{}", "a".repeat(64)))],
    );
    let source = https_source("acme", &pinned);
    // Structurally invalid as well as unpinned: the digest guard must be the
    // one that fires, or nothing proves the document went unparsed.
    let served = "this is not even TOML {{{";

    let error = MarketplaceCatalog::parse_pinned(&source, served.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::CatalogDigestMismatch {
            pinned: CatalogDigest::of_bytes(pinned.as_bytes()),
            found: CatalogDigest::of_bytes(served.as_bytes()),
        }
    );
}

#[test]
fn an_oversize_catalog_is_refused_on_size_rather_than_on_its_digest() {
    let source = https_source("acme", "unused");
    let served = "#".repeat(512 * 1024 + 1);

    let error = MarketplaceCatalog::parse_pinned(&source, served.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::CatalogTooLarge { limit: 512 * 1024 },
        "an oversize document is refused on size, not on digest"
    );
}

#[test]
fn a_catalog_whose_marketplace_identity_differs_from_its_source_is_refused() {
    let document = catalog_of(
        "evil",
        &[("evil/tools", "1.0.0", &format!("sha256:{}", "a".repeat(64)))],
    );
    let source = MarketplaceSource::new(
        marketplace("acme"),
        MarketplaceSourceKind::Https,
        "https://marketplace.example.com/catalog.toml",
        CatalogDigest::of_bytes(document.as_bytes()).as_str(),
    )
    .unwrap();

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidField {
            field: "marketplace",
            reason: "catalog identity differs from the configured marketplace source",
        }
    );
}

#[test]
fn a_catalog_cannot_offer_a_package_id_outside_its_own_namespace() {
    let document = catalog_of(
        "acme",
        &[
            ("acme/tools", "1.0.0", &format!("sha256:{}", "a".repeat(64))),
            (
                "official/heycode",
                "1.0.0",
                &format!("sha256:{}", "b".repeat(64)),
            ),
        ],
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidEntry {
            entry: 1,
            field: "packages.id",
            reason: "namespace must be the catalog's own marketplace identity",
        }
    );
}

#[test]
fn one_malformed_row_rejects_the_whole_catalog_generation() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let document = catalog_of(
        "acme",
        &[
            ("acme/tools", "1.0.0", &digest),
            ("acme/broken", "not-semver", &digest),
            ("acme/other", "2.0.0", &digest),
        ],
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidEntry {
            entry: 1,
            field: "packages.version",
            reason: "must be a valid Semantic Versioning 2.0 value",
        },
        "a partially admitted catalog would let a marketplace suppress rows by corrupting them"
    );
}

#[test]
fn the_same_id_and_version_offered_twice_rejects_the_generation() {
    let document = catalog_of(
        "acme",
        &[
            ("acme/tools", "1.0.0", &format!("sha256:{}", "a".repeat(64))),
            ("acme/tools", "1.0.0", &format!("sha256:{}", "b".repeat(64))),
        ],
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidEntry {
            entry: 1,
            field: "packages.version",
            reason: "the same id and version is offered by an earlier row",
        },
        "two digests for one pin means the pin has no single meaning"
    );
}

#[test]
fn a_remote_catalog_cannot_point_an_installer_at_a_local_path() {
    let document = format!(
        "schema_version = 1\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"1.0.0\"\nsource_kind = \"local\"\nlocator = \"fixtures/tools\"\ncontent = \"sha256:{}\"\n",
        "a".repeat(64)
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidEntry {
            entry: 0,
            field: "packages.source_kind",
            reason: "a remote catalog may not vend a local package source",
        }
    );
}

#[test]
fn a_locator_carrying_a_control_character_rejects_the_generation() {
    let document = format!(
        "schema_version = 1\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"1.0.0\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/\\u0007tools\"\ncontent = \"sha256:{}\"\n",
        "a".repeat(64)
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidEntry {
            entry: 0,
            field: "packages.locator",
            reason: "must be a bounded public locator without credentials, query strings, or traversal",
        }
    );
}

#[test]
fn a_catalog_declaring_more_rows_than_the_budget_is_refused() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let mut document = "schema_version = 1\nmarketplace = \"acme\"\n".to_owned();
    for index in 0..=1_024 {
        document.push_str(&format!(
            "\n[[packages]]\nid = \"acme/p{index}\"\nversion = \"1.0.0\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/p{index}\"\ncontent = \"{digest}\"\n",
        ));
    }
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(error, MarketplaceError::TooManyEntries { limit: 1_024 });
}

#[test]
fn a_catalog_from_a_newer_schema_is_refused_rather_than_read_partially() {
    let document = format!(
        "schema_version = 2\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"1.0.0\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/tools\"\ncontent = \"sha256:{}\"\n",
        "a".repeat(64)
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::UnsupportedCatalogSchema {
            found: 2,
            supported: MARKETPLACE_CATALOG_SCHEMA_VERSION,
        }
    );
}

#[test]
fn a_rejected_catalog_never_echoes_its_own_bytes_into_the_failure() {
    const CANARY: &str = "canary-9f3a1c-do-not-echo";
    let document = format!(
        "schema_version = 1\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"not-semver\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/{CANARY}\"\ncontent = \"sha256:{}\"\n",
        "a".repeat(64)
    );
    assert!(
        document.contains(CANARY),
        "the fixture must actually carry the canary or this test proves nothing"
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert!(!format!("{error}").contains(CANARY));
    assert!(!format!("{error:?}").contains(CANARY));
}

#[test]
fn an_admitted_catalog_reports_the_exact_document_it_was_parsed_from() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let document = catalog_of("acme", &[("acme/tools", "1.0.0", &digest)]);
    let source = https_source("acme", &document);

    let catalog = parse(&source, &document);
    assert_eq!(catalog.schema_version(), MARKETPLACE_CATALOG_SCHEMA_VERSION);
    assert_eq!(catalog.marketplace(), &marketplace("acme"));
    assert_eq!(catalog.digest(), source.catalog_digest());
    assert_eq!(catalog.entries().len(), 1);
    assert_eq!(catalog.entries()[0].id(), &id("acme/tools"));
    assert_eq!(catalog.entries()[0].content().as_str(), digest);
    assert_eq!(catalog.entries()[0].source_kind(), PluginSourceKind::Https);
}

#[test]
fn a_pin_the_catalog_does_not_offer_resolves_to_nothing_rather_than_a_neighbour() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let document = catalog_of(
        "acme",
        &[
            ("acme/tools", "1.0.0", &digest),
            ("acme/tools", "2.0.0", &digest),
        ],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    assert!(catalog.pin(&id("acme/tools"), &version("1.5.0")).is_none());
    assert_eq!(
        catalog
            .pin(&id("acme/tools"), &version("2.0.0"))
            .unwrap()
            .version(),
        &version("2.0.0")
    );
}

// --- Signature -----------------------------------------------------------

#[test]
fn a_declared_signature_is_recorded_as_present_and_never_as_verified() {
    let document = format!(
        "schema_version = 1\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"1.0.0\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/tools\"\ncontent = \"sha256:{}\"\n\n[packages.signature]\nalgorithm = \"ed25519\"\nkey_id = \"acme.release.2026\"\nvalue = \"{CANONICAL_SIGNATURE}\"\n",
        "a".repeat(64)
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    // The only two states this crate can produce. `Present` says a canonical
    // signature was recorded; nothing here verified it, and no variant claims
    // otherwise.
    match catalog.entries()[0].signature() {
        SignatureState::Present(signature) => {
            assert_eq!(signature.key_id, "acme.release.2026");
            assert_eq!(signature.value, CANONICAL_SIGNATURE);
        }
        SignatureState::Absent => panic!("a declared signature must be recorded"),
    }
}

#[test]
fn a_catalog_row_with_no_signature_is_absent_rather_than_empty() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let document = catalog_of("acme", &[("acme/tools", "1.0.0", &digest)]);
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    assert_eq!(catalog.entries()[0].signature(), &SignatureState::Absent);
}

#[test]
fn a_signature_that_is_not_canonical_ed25519_rejects_the_generation() {
    let document = format!(
        "schema_version = 1\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"1.0.0\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/tools\"\ncontent = \"sha256:{}\"\n\n[packages.signature]\nalgorithm = \"ed25519\"\nkey_id = \"acme.release.2026\"\nvalue = \"not-a-signature\"\n",
        "a".repeat(64)
    );
    let source = https_source("acme", &document);

    let error = MarketplaceCatalog::parse_pinned(&source, document.as_bytes()).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::InvalidEntry {
            entry: 0,
            field: "packages.signature",
            reason: "must be a stable key reference and canonical Ed25519 base64",
        }
    );
}

// --- Pin ------------------------------------------------------------------

#[test]
fn a_pinned_version_never_silently_upgrades_to_another_offered_version() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, delivered) = install(&temp, "delivered", "acme/tools", "2.0.0", "# Two\n");
    let document = catalog_of(
        "acme",
        &[
            ("acme/tools", "1.0.0", &format!("sha256:{}", "a".repeat(64))),
            ("acme/tools", "2.0.0", delivered.content_hash().as_str()),
        ],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    // 2.0.0 is genuinely offered, and its digest genuinely matches what
    // arrived. The pin still refuses, because 1.0.0 is what was asked for.
    let error = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &delivered)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "pinned `acme/tools` version `1.0.0` installed as version `2.0.0`"
    );
    let MarketplaceError::Substituted(substitution) = error else {
        panic!("a refused pin must be reported as a substitution");
    };
    assert!(matches!(*substitution, Substitution::Version { .. }));
}

#[test]
fn a_pin_the_catalog_cannot_satisfy_refuses_instead_of_admitting_what_arrived() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, delivered) = install(&temp, "delivered", "acme/tools", "1.0.0", "# One\n");
    let document = catalog_of(
        "acme",
        &[("acme/other", "1.0.0", delivered.content_hash().as_str())],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    let error = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &delivered)
        .unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::NotOffered {
            id: id("acme/tools"),
            version: version("1.0.0"),
        }
    );
}

/// An unsatisfiable pin and a detected substitution are different operator
/// situations. With no row for the requested version there is no digest to
/// verify against, so the honest report is that the pin cannot be resolved —
/// calling it a substitution would announce an attack this crate has no
/// evidence for, the same lie as reporting an unverified signature as valid.
#[test]
fn a_pin_the_catalog_never_offered_is_unresolvable_rather_than_a_substitution() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, delivered) = install(&temp, "delivered", "acme/tools", "2.0.0", "# Two\n");
    let document = catalog_of(
        "acme",
        &[("acme/tools", "2.0.0", delivered.content_hash().as_str())],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    let error = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &delivered)
        .unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::NotOffered {
            id: id("acme/tools"),
            version: version("1.0.0"),
        },
        "the 2.0.0 row must not be selected on the installed package's behalf"
    );
}

#[test]
fn a_package_whose_identity_differs_from_the_pin_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, delivered) = install(&temp, "delivered", "acme/other", "1.0.0", "# Other\n");
    let document = catalog_of(
        "acme",
        &[
            ("acme/tools", "1.0.0", delivered.content_hash().as_str()),
            ("acme/other", "1.0.0", delivered.content_hash().as_str()),
        ],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    let error = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &delivered)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "pinned plugin `acme/tools` installed as `acme/other`"
    );
    let MarketplaceError::Substituted(substitution) = error else {
        panic!("a refused pin must be reported as a substitution");
    };
    assert!(matches!(*substitution, Substitution::Identity { .. }));
}

// --- Substitution ---------------------------------------------------------

#[test]
fn bytes_that_miss_the_pinned_digest_are_refused_although_the_cache_itself_accepts_them() {
    let temp = tempfile::tempdir().unwrap();
    let (_published_cache, published) =
        install(&temp, "published", "acme/tools", "1.0.0", "# Good\n");
    let (substituted_cache, substituted) =
        install(&temp, "substituted", "acme/tools", "1.0.0", "# Evil\n");
    assert_ne!(published.content_hash(), substituted.content_hash());

    let document = catalog_of(
        "acme",
        &[("acme/tools", "1.0.0", published.content_hash().as_str())],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    // PL02 is entirely happy: it installed these bytes and re-resolves them
    // against its own committed reference without complaint. Self-consistency
    // is not publisher verification — whoever wrote the cache wrote both
    // halves of that comparison.
    let resolved = substituted_cache
        .resolve(&id("acme/tools"), &version("1.0.0"))
        .unwrap();
    assert_eq!(resolved.content_hash(), substituted.content_hash());

    let error = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &substituted)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "pinned `acme/tools` version `1.0.0` content {} does not match pinned {}",
            substituted.content_hash(),
            published.content_hash()
        )
    );
    let MarketplaceError::Substituted(substitution) = error else {
        panic!("substituted bytes must be reported as a substitution");
    };
    assert!(matches!(*substitution, Substitution::Content { .. }));
}

#[test]
fn a_recorded_provenance_refuses_bytes_that_changed_after_it_was_established() {
    let temp = tempfile::tempdir().unwrap();
    let (_published_cache, published) =
        install(&temp, "published", "acme/tools", "1.0.0", "# Good\n");
    let (_substituted_cache, substituted) =
        install(&temp, "substituted", "acme/tools", "1.0.0", "# Evil\n");
    let document = catalog_of(
        "acme",
        &[("acme/tools", "1.0.0", published.content_hash().as_str())],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    let provenance = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &published)
        .unwrap();
    provenance.reverify(&published).unwrap();

    // A digest compared once at install time and never again would pass this
    // package as the one that was verified.
    let error = provenance.reverify(&substituted).unwrap_err();
    assert_eq!(
        error,
        MarketplaceError::Substituted(Box::new(Substitution::Content {
            id: id("acme/tools"),
            version: version("1.0.0"),
            pinned: published.content_hash().clone(),
            installed: substituted.content_hash().clone(),
        }))
    );
}

// --- Provenance -----------------------------------------------------------

#[test]
fn admitted_provenance_records_the_marketplace_the_digest_and_the_signature_state() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, published) = install(&temp, "published", "acme/tools", "1.0.0", "# Good\n");
    let document = format!(
        "schema_version = 1\nmarketplace = \"acme\"\n\n[[packages]]\nid = \"acme/tools\"\nversion = \"1.0.0\"\nsource_kind = \"https\"\nlocator = \"https://packages.example.com/tools\"\ncontent = \"{}\"\n\n[packages.signature]\nalgorithm = \"ed25519\"\nkey_id = \"acme.release.2026\"\nvalue = \"{CANONICAL_SIGNATURE}\"\n",
        published.content_hash().as_str()
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);

    let provenance = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &published)
        .unwrap();
    assert_eq!(
        provenance.origin(),
        &PackageOrigin::Marketplace {
            marketplace: marketplace("acme")
        }
    );
    assert!(provenance.is_established());
    assert_eq!(provenance.id(), &id("acme/tools"));
    assert_eq!(provenance.version(), &version("1.0.0"));
    assert_eq!(provenance.content(), published.content_hash());
    assert!(matches!(provenance.signature(), SignatureState::Present(_)));
}

/// Provenance is only useful if it can be reported. The projection is pinned
/// because a surface renders exactly these keys, and because an origin that
/// cannot be serialized is an origin nobody can show.
#[test]
fn provenance_projects_a_deterministic_reportable_record() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, published) = install(&temp, "published", "acme/tools", "1.0.0", "# Good\n");
    let document = catalog_of(
        "acme",
        &[("acme/tools", "1.0.0", published.content_hash().as_str())],
    );
    let source = https_source("acme", &document);
    let catalog = parse(&source, &document);
    let provenance = catalog
        .admit(&id("acme/tools"), &version("1.0.0"), &published)
        .unwrap();

    assert_eq!(
        serde_json::to_value(&provenance).unwrap(),
        serde_json::json!({
            "origin": {"kind": "marketplace", "marketplace": "acme"},
            "id": "acme/tools",
            "version": "1.0.0",
            "content": published.content_hash().as_str(),
            "signature": {"state": "absent"},
            "claimed_source_kind": "local",
        })
    );
    assert_eq!(
        serde_json::to_value(PackageProvenance::unknown(&published).origin()).unwrap(),
        serde_json::json!({"kind": "unknown"})
    );
}

#[test]
fn a_package_claiming_a_marketplace_origin_is_still_unknown_without_host_held_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = temp.path().join("claimant-source");
    write_claiming_package(
        &source_root,
        "acme/tools",
        "1.0.0",
        "# Claim\n",
        "marketplace",
        "acme/tools",
    );
    let cache = PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap();
    let installed = cache.install_directory(&source_root).unwrap();

    let provenance = PackageProvenance::unknown(&installed);
    assert_eq!(
        provenance.claimed_source_kind(),
        PluginSourceKind::Marketplace,
        "the package's own claim is retained so a surface can show it"
    );
    assert_eq!(
        provenance.origin(),
        &PackageOrigin::Unknown,
        "a claim by the package about itself is not evidence about the package"
    );
    assert!(!provenance.is_established());
}

#[test]
fn unknown_provenance_stays_unknown_across_re_verification() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, installed) = install(&temp, "installed", "acme/tools", "1.0.0", "# Good\n");

    let provenance = PackageProvenance::unknown(&installed);
    provenance.reverify(&installed).unwrap();
    assert_eq!(provenance.origin(), &PackageOrigin::Unknown);
    assert!(!provenance.is_established());
}

#[test]
fn an_operator_named_path_establishes_provenance_that_still_detects_changed_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let (_cache, installed) = install(&temp, "installed", "acme/tools", "1.0.0", "# Good\n");
    let (_other_cache, changed) = install(&temp, "changed", "acme/tools", "1.0.0", "# Changed\n");

    let provenance = PackageProvenance::operator_path(&installed);
    assert_eq!(provenance.origin(), &PackageOrigin::OperatorPath);
    assert!(provenance.is_established());
    assert_eq!(
        provenance.signature(),
        &SignatureState::Absent,
        "a path the operator named carries no publisher signature"
    );

    let error = provenance.reverify(&changed).unwrap_err();
    let MarketplaceError::Substituted(substitution) = error else {
        panic!("changed bytes must be reported as a substitution");
    };
    assert!(matches!(*substitution, Substitution::Content { .. }));
}

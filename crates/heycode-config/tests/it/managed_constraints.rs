//! K11 managed profile constraints reject forbidden implementations before apply.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use heycode_config::{
    ManagedCodeGrant, ManagedCodeRuntime, ManagedCodeSessionPolicy, ManagedWasiPreopenAccess,
    PluginFactories, ProfileDocument, ProfileLayer, ProfileSource, resolve_profile_tree,
};
use heycode_core::{CoreResult, Plugin, PluginContributionKind, PluginDescriptor, PluginScope};

struct CountedPlugin {
    id: &'static str,
    descriptor: PluginDescriptor,
    applies: Arc<AtomicUsize>,
}

impl Plugin for CountedPlugin {
    fn name(&self) -> &'static str {
        self.id
    }

    fn descriptor(&self) -> PluginDescriptor {
        self.descriptor
    }

    fn apply(&self, _context: &mut heycode_core::Context) -> CoreResult<()> {
        self.applies.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn managed(raw: &str) -> ProfileLayer {
    ProfileLayer::new(
        PluginScope::Managed,
        ProfileSource::managed("fleet-policy"),
        ProfileDocument::from_toml(raw).unwrap(),
    )
    .unwrap()
}

#[test]
fn forbidden_source_is_rejected_before_the_plugin_can_activate() {
    let tree = resolve_profile_tree(
        &["legacy"],
        &[managed(
            r#"
schema_version = 2
plugins = []

[constraints]
allowed_sources = ["built_in"]
"#,
        )],
    )
    .unwrap();
    let applies = Arc::new(AtomicUsize::new(0));
    let mut factories = PluginFactories::new();
    let observed = applies.clone();
    factories.register("legacy", move || {
        Box::new(CountedPlugin {
            id: "legacy",
            descriptor: PluginDescriptor::unclassified("legacy"),
            applies: observed,
        })
    });

    let error = factories
        .build_profile(&tree)
        .err()
        .expect("forbidden source must fail")
        .to_string();
    assert!(error.contains("legacy"), "{error}");
    assert!(error.contains("unclassified"), "{error}");
    assert_eq!(applies.load(Ordering::SeqCst), 0);
}

#[test]
fn forbidden_capability_is_rejected_before_the_plugin_can_activate() {
    let tree = resolve_profile_tree(
        &["runner"],
        &[managed(
            r#"
schema_version = 2
plugins = []

[constraints]
denied_capabilities = ["external_process"]
"#,
        )],
    )
    .unwrap();
    let applies = Arc::new(AtomicUsize::new(0));
    let mut factories = PluginFactories::new();
    let observed = applies.clone();
    factories.register("runner", move || {
        Box::new(CountedPlugin {
            id: "runner",
            descriptor: PluginDescriptor::built_in(
                "runner",
                "1.0.0",
                &[PluginContributionKind::ExternalProcess],
            ),
            applies: observed,
        })
    });

    let error = factories
        .build_profile(&tree)
        .err()
        .expect("forbidden capability must fail")
        .to_string();
    assert!(error.contains("runner"), "{error}");
    assert!(error.contains("external_process"), "{error}");
    assert_eq!(applies.load(Ordering::SeqCst), 0);
}

#[test]
fn only_managed_authority_can_declare_constraints() {
    let document = ProfileDocument::from_toml(
        r#"
schema_version = 2
plugins = []

[constraints]
allowed_sources = ["built_in"]
"#,
    )
    .unwrap();
    let error = ProfileLayer::new(
        PluginScope::User,
        ProfileSource::user("/home/test/profile.toml"),
        document,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("managed"), "{error}");
}

#[test]
fn admitted_built_in_capabilities_preserve_exact_composition_order() {
    let tree = resolve_profile_tree(
        &["settings", "doctor"],
        &[managed(
            r#"
schema_version = 2
plugins = []

[constraints]
allowed_sources = ["built_in"]
denied_capabilities = ["external_process"]
"#,
        )],
    )
    .unwrap();
    let applies = Arc::new(AtomicUsize::new(0));
    let mut factories = PluginFactories::new();
    for (id, contributions) in [
        ("settings", &[PluginContributionKind::Service][..]),
        ("doctor", &[PluginContributionKind::Diagnostic][..]),
    ] {
        let observed = applies.clone();
        factories.register(id, move || {
            Box::new(CountedPlugin {
                id,
                descriptor: PluginDescriptor::built_in(id, "1.0.0", contributions),
                applies: observed,
            })
        });
    }

    let plugins = factories.build_profile(&tree).unwrap();
    assert_eq!(
        plugins
            .iter()
            .map(|plugin| plugin.plugin().name())
            .collect::<Vec<_>>(),
        ["settings", "doctor"]
    );
    assert_eq!(applies.load(Ordering::SeqCst), 0);
    let context = heycode_core::compose_scoped(&plugins).unwrap();
    assert_eq!(context.plugins(), ["settings", "doctor"]);
    assert_eq!(applies.load(Ordering::SeqCst), 2);
}

#[test]
fn managed_profile_v3_parses_one_exact_code_authority_generation() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let raw = format!(
        r#"
schema_version = 3
plugins = []

[code_authority]
pl08_generation = "{digest}"

[[code_authority.packages]]
id = "acme/native-reviewer"
version = "1.2.3"
package_digest = "{digest}"
runtime = "native_process"
entrypoint = "bin/reviewer"
session = "unique_per_activation"
grants = ["filesystem_read", "process_spawn"]

[[code_authority.packages]]
id = "acme/wasi-reviewer"
version = "2.0.0"
package_digest = "{digest}"
runtime = "wasi_component_v1"
entrypoint = "components/reviewer.wasm"
session = "unique_per_activation"
grants = ["filesystem_read", "network_access"]

[[code_authority.packages.preopens]]
host_path = "/srv/heycode/review-input"
guest_path = "/input"
access = "read_only"

[[code_authority.packages.network_endpoints]]
address = "203.0.113.7"
port = 443
"#,
    );

    let layer = managed(&raw);
    let authority = layer.document.code_authority.clone().unwrap();
    assert_eq!(authority.pl08_generation, digest);
    assert_eq!(authority.packages.len(), 2);
    assert_eq!(
        authority.packages[0].runtime,
        ManagedCodeRuntime::NativeProcess
    );
    assert_eq!(
        authority.packages[0].session,
        ManagedCodeSessionPolicy::UniquePerActivation
    );
    assert_eq!(
        authority.packages[0].grants,
        [
            ManagedCodeGrant::FilesystemRead,
            ManagedCodeGrant::ProcessSpawn
        ]
    );
    assert_eq!(
        authority.packages[1].preopens[0].access,
        ManagedWasiPreopenAccess::ReadOnly
    );

    let tree = resolve_profile_tree(&["product-extensions"], &[layer]).unwrap();
    assert_eq!(tree.code_authority, Some(authority));
}

#[test]
fn user_project_and_session_layers_cannot_self_grant_code_authority() {
    let raw = format!(
        r#"
schema_version = 3
plugins = []

[code_authority]
pl08_generation = "sha256:{}"
packages = []
"#,
        "b".repeat(64)
    );
    let document = ProfileDocument::from_toml(&raw).unwrap();
    for (scope, source) in [
        (
            PluginScope::User,
            ProfileSource::user("/home/test/profile.toml"),
        ),
        (
            PluginScope::Project,
            ProfileSource::project("/workspace/profile.toml"),
        ),
        (
            PluginScope::Session,
            ProfileSource::session("turn-override"),
        ),
    ] {
        let error = ProfileLayer::new(scope, source, document.clone())
            .unwrap_err()
            .to_string();
        assert!(error.contains("managed"), "{scope:?}: {error}");
    }
}

#[test]
fn code_authority_requires_v3_and_rejects_ambiguous_or_widening_rows() {
    let digest = format!("sha256:{}", "c".repeat(64));
    let cases = [
        format!(
            "schema_version = 2\nplugins = []\n[code_authority]\npl08_generation = \"{digest}\"\npackages = []\n"
        ),
        format!(
            r#"
schema_version = 3
plugins = []
[code_authority]
pl08_generation = "{digest}"
[[code_authority.packages]]
id = "acme/reviewer"
version = "1.0.0"
package_digest = "{digest}"
runtime = "native_process"
entrypoint = "bin/reviewer"
session = "unique_per_activation"
grants = ["process_spawn", "process_spawn"]
"#
        ),
        format!(
            r#"
schema_version = 3
plugins = []
[code_authority]
pl08_generation = "{digest}"
[[code_authority.packages]]
id = "acme/reviewer"
version = "1.0.0"
package_digest = "{digest}"
runtime = "native_process"
entrypoint = "bin/reviewer"
session = "unique_per_activation"
grants = []
[[code_authority.packages.network_endpoints]]
address = "203.0.113.7"
port = 443
"#
        ),
        format!(
            r#"
schema_version = 3
plugins = []
[code_authority]
pl08_generation = "{digest}"
[[code_authority.packages]]
id = "acme/reviewer"
version = "1.0.0"
package_digest = "{digest}"
runtime = "wasi_component_v1"
entrypoint = "components/reviewer.wasm"
session = "unique_per_activation"
grants = ["filesystem_read"]
[[code_authority.packages.preopens]]
host_path = "relative/input"
guest_path = "/input"
access = "read_only"
"#
        ),
        format!(
            r#"
schema_version = 3
plugins = []
[code_authority]
pl08_generation = "{digest}"
[[code_authority.packages]]
id = "acme/reviewer"
version = "1.0.0"
package_digest = "{digest}"
runtime = "wasi_component_v1"
entrypoint = "components/reviewer.wasm"
session = "unique_per_activation"
grants = ["filesystem_write"]
[[code_authority.packages.preopens]]
host_path = "/srv/heycode/output"
guest_path = "/output"
access = "write_only"
"#
        ),
    ];

    for raw in cases {
        assert!(ProfileDocument::from_toml(&raw).is_err(), "accepted: {raw}");
    }
}

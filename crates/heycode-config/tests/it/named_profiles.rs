//! K07 named profile store shared by CLI and picker consumers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{
    NamedProfileService, NamedProfileStore, SERVICE_PROFILES, named_profiles_plugin,
};
use heycode_core::{PluginScope, compose};

fn profile(name: &str, plugin: &str, enabled: bool) -> String {
    format!(
        "schema_version = 1\nname = \"{name}\"\n\n[[plugins]]\nid = \"{plugin}\"\nenabled = {enabled}\n"
    )
}

#[test]
fn list_is_sorted_and_load_returns_the_same_strict_user_layer() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("profiles");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("beta.toml"), profile("beta", "mcp", false)).unwrap();
    std::fs::write(root.join("alpha.toml"), profile("alpha", "skills", false)).unwrap();
    let store = NamedProfileStore::new(home.path());
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    let layer = store.load("alpha").unwrap();
    assert_eq!(layer.scope, PluginScope::User);
    assert_eq!(layer.source.kind(), "named_profile");
    assert_eq!(layer.document.name.as_deref(), Some("alpha"));
    assert_eq!(layer.document.plugins[0].id, "skills");
    assert!(!layer.document.plugins[0].enabled);
}

#[test]
fn traversal_name_mismatch_and_oversized_profile_fail_loud() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("profiles");
    std::fs::create_dir_all(&root).unwrap();
    let store = NamedProfileStore::new(home.path());
    assert!(store.load("../escape").is_err());

    std::fs::write(
        root.join("expected.toml"),
        profile("different", "mcp", false),
    )
    .unwrap();
    assert!(store.load("expected").is_err());
    std::fs::write(root.join("huge.toml"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(store.load("huge").is_err());
}

#[cfg(unix)]
#[test]
fn symbolic_link_profile_is_refused() {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("profiles");
    std::fs::create_dir_all(&root).unwrap();
    let target = home.path().join("outside.toml");
    std::fs::write(&target, profile("linked", "mcp", false)).unwrap();
    symlink(&target, root.join("linked.toml")).unwrap();
    assert!(NamedProfileStore::new(home.path()).load("linked").is_err());
}

#[test]
fn picker_service_uses_the_same_store_and_stops_with_its_effect() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("profiles");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("alpha.toml"), profile("alpha", "skills", false)).unwrap();
    let mut context = compose(&[named_profiles_plugin(home.path())]).unwrap();
    let service = context
        .get::<NamedProfileService>(SERVICE_PROFILES)
        .expect("picker service is composed");

    assert_eq!(service.list().unwrap()[0].name, "alpha");
    assert_eq!(
        service.load("alpha").unwrap(),
        NamedProfileStore::new(home.path()).load("alpha").unwrap(),
        "CLI and picker must receive the same exact layer"
    );

    context.shutdown();
    assert!(
        service.list().is_err(),
        "held service must stop with its plugin"
    );
}

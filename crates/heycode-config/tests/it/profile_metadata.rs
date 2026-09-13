//! K06 versioned profile documents and source-aware effective tree.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{
    PROFILE_SCHEMA_VERSION, ProfileDocument, ProfileLayer, ProfileSource, resolve_profile_tree,
};
use heycode_core::PluginScope;

#[test]
fn effective_tree_retains_every_layer_source_and_decision() {
    let user = ProfileDocument::from_toml(
        r#"
schema_version = 1
name = "user-default"

[[plugins]]
id = "mcp"
enabled = false

[[plugins]]
id = "custom"
enabled = true
"#,
    )
    .unwrap();
    let project = ProfileDocument::from_toml(
        r#"
schema_version = 1

[[plugins]]
id = "mcp"
enabled = true
"#,
    )
    .unwrap();
    let layers = vec![
        ProfileLayer::new(
            PluginScope::Project,
            ProfileSource::project("/workspace/heycode.profile.toml"),
            project,
        )
        .unwrap(),
        ProfileLayer::new(
            PluginScope::User,
            ProfileSource::user("/home/test/.heycode/profile.toml"),
            user,
        )
        .unwrap(),
    ];
    let tree = resolve_profile_tree(&["settings", "tools", "mcp"], &layers).unwrap();
    assert_eq!(tree.schema_version, PROFILE_SCHEMA_VERSION);
    assert_eq!(
        tree.layers
            .iter()
            .map(|layer| (layer.scope, layer.source.kind()))
            .collect::<Vec<_>>(),
        [
            (PluginScope::BuiltIn, "built_in"),
            (PluginScope::User, "user_file"),
            (PluginScope::Project, "project_file"),
        ]
    );
    assert_eq!(
        tree.enabled
            .iter()
            .map(|row| (row.id.as_str(), row.scope, row.source.kind()))
            .collect::<Vec<_>>(),
        [
            ("settings", PluginScope::BuiltIn, "built_in"),
            ("tools", PluginScope::BuiltIn, "built_in"),
            ("custom", PluginScope::User, "user_file"),
            ("mcp", PluginScope::Project, "project_file"),
        ]
    );
    let mcp = tree.plugins.iter().find(|row| row.id == "mcp").unwrap();
    assert!(mcp.enabled);
    assert_eq!(mcp.decisions.len(), 3);
    assert_eq!(mcp.decisions[0].source.kind(), "built_in");
    assert!(mcp.decisions[0].enabled);
    assert_eq!(mcp.decisions[1].source.kind(), "user_file");
    assert!(!mcp.decisions[1].enabled);
    assert_eq!(mcp.decisions[2].source.kind(), "project_file");
    assert!(mcp.decisions[2].enabled);
}

#[test]
fn profile_version_shape_and_scope_source_mismatch_fail_loud() {
    for raw in [
        "schema_version = 4\nplugins = []\n",
        "schema_version = 1\nunknown = true\nplugins = []\n",
        "schema_version = 1\n[[plugins]]\nid = \"Bad_Id\"\n",
        "schema_version = 1\n[[plugins]]\nid = \"same\"\n[[plugins]]\nid = \"same\"\n",
    ] {
        assert!(ProfileDocument::from_toml(raw).is_err(), "accepted: {raw}");
    }

    let doc = ProfileDocument::from_toml("schema_version = 1\nplugins = []\n").unwrap();
    let error = ProfileLayer::new(
        PluginScope::Project,
        ProfileSource::user("/home/test/profile.toml"),
        doc,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("project"), "{error}");
    assert!(error.contains("user_file"), "{error}");

    let doc = ProfileDocument::from_toml("schema_version = 1\nplugins = []\n").unwrap();
    assert!(ProfileLayer::new(PluginScope::Session, ProfileSource::session(" "), doc).is_err());
}

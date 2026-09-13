//! MCP10: the seven operations, and the parity mechanism that keeps surfaces
//! from drifting apart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_mcp::McpTransportKind;
use heycode_mcp::management::{
    McpDefinitionStore, McpHealth, McpManagement, McpManagementError, McpOperation, McpProbe,
    McpStatusRow, StoredServer,
};

#[derive(Default)]
struct MemoryStore {
    servers: Mutex<BTreeMap<String, StoredServer>>,
    writes: Mutex<usize>,
}

impl McpDefinitionStore for MemoryStore {
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        Ok(self.servers.lock().unwrap().clone())
    }
    fn persist(&self, servers: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> {
        *self.servers.lock().unwrap() = servers.clone();
        *self.writes.lock().unwrap() += 1;
        Ok(())
    }
}

struct Fixed(McpHealth);
impl McpProbe for Fixed {
    fn probe(&self, _server: &StoredServer) -> McpHealth {
        self.0
    }
}

fn stdio(name: &str) -> StoredServer {
    StoredServer::new(name, McpTransportKind::Stdio, "/usr/bin/some-server").unwrap()
}

fn management() -> (Arc<MemoryStore>, McpManagement) {
    let store = Arc::new(MemoryStore::default());
    let management = McpManagement::new(store.clone() as Arc<dyn McpDefinitionStore>);
    (store, management)
}

/// The parity mechanism itself. If someone adds an eighth operation, `ALL` no
/// longer covers the enum and this fails — before any surface can quietly not
/// implement it.
#[test]
fn every_operation_is_listed_in_all_and_round_trips_through_its_word() {
    assert_eq!(McpOperation::ALL.len(), 7);
    let mut seen = std::collections::BTreeSet::new();
    for operation in McpOperation::ALL {
        assert!(seen.insert(operation), "duplicate in ALL: {operation}");
        assert_eq!(McpOperation::parse(operation.as_str()), Some(operation));
    }
    // The acceptance criterion names these seven words exactly.
    let words: Vec<_> = McpOperation::ALL.iter().map(|o| o.as_str()).collect();
    assert_eq!(
        words,
        vec!["add", "list", "auth", "test", "edit", "enable", "remove"]
    );
    assert_eq!(McpOperation::parse("nonsense"), None);
}

/// Exactly the mutating operations are the ones a managed definition refuses.
#[test]
fn mutating_operations_are_exactly_add_edit_enable_remove() {
    let mutating: Vec<_> = McpOperation::ALL
        .into_iter()
        .filter(|operation| operation.mutates())
        .collect();
    assert_eq!(
        mutating,
        vec![
            McpOperation::Add,
            McpOperation::Edit,
            McpOperation::Enable,
            McpOperation::Remove
        ]
    );
}

#[test]
fn add_then_list_shows_the_server() {
    let (_store, management) = management();
    management.add(stdio("files")).unwrap();

    let rows = management.list().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].server.name, "files");
    assert!(rows[0].server.enabled, "a new server is enabled by default");
    assert_eq!(
        rows[0].health,
        McpHealth::Unknown,
        "with no probe installed, health is unknown rather than assumed"
    );
}

/// Adding over an existing name must not silently overwrite — `edit` is the
/// operation that changes a server, and conflating them loses a definition.
#[test]
fn add_refuses_a_duplicate_name_rather_than_overwriting() {
    let (store, management) = management();
    management.add(stdio("files")).unwrap();
    let mut second = stdio("files");
    second.target = "/usr/bin/different".to_owned();

    assert_eq!(
        management.add(second).unwrap_err(),
        McpManagementError::DuplicateServer("files".to_owned())
    );
    assert_eq!(
        store.load().unwrap()["files"].target,
        "/usr/bin/some-server",
        "the original definition must survive a rejected add"
    );
}

#[test]
fn add_refuses_every_non_user_scope_before_the_store_can_publish_it() {
    for scope in [
        heycode_mcp::McpDefinitionScope::Project,
        heycode_mcp::McpDefinitionScope::Local,
        heycode_mcp::McpDefinitionScope::Managed,
        heycode_mcp::McpDefinitionScope::Plugin,
    ] {
        let (store, management) = management();
        let mut server = stdio("foreign");
        server.scope = scope;
        let expected = if scope == heycode_mcp::McpDefinitionScope::Managed {
            McpManagementError::Managed("foreign".to_owned())
        } else {
            McpManagementError::ReadOnlyScope {
                name: "foreign".to_owned(),
                scope,
            }
        };
        assert_eq!(management.add(server).unwrap_err(), expected,);
        assert!(store.load().unwrap().is_empty());
        assert_eq!(*store.writes.lock().unwrap(), 0);
    }
}

#[test]
fn enable_toggles_without_losing_the_definition() {
    let (_store, management) = management();
    management.add(stdio("files")).unwrap();

    management.enable("files", false).unwrap();
    let rows = management.list().unwrap();
    assert!(!rows[0].server.enabled);
    assert_eq!(rows[0].server.target, "/usr/bin/some-server");

    management.enable("files", true).unwrap();
    assert!(management.list().unwrap()[0].server.enabled);
}

/// Editing one field must not quietly reset the others — an edit that
/// re-enables a server the user deliberately turned off is a real incident.
#[test]
fn edit_changes_only_the_target_and_preserves_enabled_state() {
    let (_store, management) = management();
    management.add(stdio("files")).unwrap();
    management.enable("files", false).unwrap();

    management
        .edit("files", McpTransportKind::Stdio, "/usr/bin/new-server")
        .unwrap();

    let row = &management.list().unwrap()[0];
    assert_eq!(row.server.target, "/usr/bin/new-server");
    assert!(
        !row.server.enabled,
        "an edit must not re-enable a disabled server"
    );
}

/// A user who misspells a name must not believe they removed a server that is
/// still running.
#[test]
fn removing_an_unknown_server_is_reported_not_silently_successful() {
    let (store, management) = management();
    management.add(stdio("files")).unwrap();

    assert_eq!(
        management.remove("fils").unwrap_err(),
        McpManagementError::UnknownServer("fils".to_owned())
    );
    assert_eq!(store.load().unwrap().len(), 1);

    management.remove("files").unwrap();
    assert!(store.load().unwrap().is_empty());
    assert!(management.list().unwrap().is_empty());
}

#[test]
fn every_operation_on_an_unknown_server_names_that_server() {
    let (_store, management) = management();
    for error in [
        management.auth("ghost").unwrap_err(),
        management.test("ghost").unwrap_err(),
        management
            .edit("ghost", McpTransportKind::Stdio, "/bin/x")
            .unwrap_err(),
        management.enable("ghost", true).unwrap_err(),
        management.remove("ghost").unwrap_err(),
    ] {
        assert_eq!(error, McpManagementError::UnknownServer("ghost".to_owned()));
    }
}

/// An administrator-managed definition refuses exactly the mutating operations
/// and permits the read-only ones — a user must still be able to see and probe
/// a server they are not allowed to change.
#[test]
fn a_managed_definition_refuses_mutation_but_still_lists_and_probes() {
    let store = Arc::new(MemoryStore::default());
    let mut managed = stdio("corp");
    managed.managed = true;
    store
        .persist(&BTreeMap::from([("corp".to_owned(), managed)]))
        .unwrap();
    let management = McpManagement::new(store.clone() as Arc<dyn McpDefinitionStore>)
        .with_probe(Arc::new(Fixed(McpHealth::Reachable)));

    for error in [
        management
            .edit("corp", McpTransportKind::Stdio, "/bin/evil")
            .unwrap_err(),
        management.enable("corp", false).unwrap_err(),
        management.remove("corp").unwrap_err(),
    ] {
        assert_eq!(error, McpManagementError::Managed("corp".to_owned()));
    }
    assert_eq!(management.list().unwrap().len(), 1);
    assert_eq!(management.test("corp").unwrap(), McpHealth::Reachable);
    assert_eq!(store.load().unwrap()["corp"].target, "/usr/bin/some-server");
}

/// "I did not check" and "I checked and it is down" are different facts, and
/// only one of them should make a user go restart a server.
#[test]
fn health_is_unknown_without_a_probe_and_never_unreachable() {
    let (_store, management) = management();
    management.add(stdio("files")).unwrap();
    assert_eq!(management.test("files").unwrap(), McpHealth::Unknown);
    assert_eq!(management.auth("files").unwrap(), McpHealth::Unknown);
}

/// Authorization-required is its own answer: it needs `auth`, not a restart.
#[test]
fn a_probe_can_distinguish_authorization_required_from_unreachable() {
    for health in [
        McpHealth::Reachable,
        McpHealth::Unreachable,
        McpHealth::AuthorizationRequired,
    ] {
        let store = Arc::new(MemoryStore::default());
        let management = McpManagement::new(store as Arc<dyn McpDefinitionStore>)
            .with_probe(Arc::new(Fixed(health)));
        management.add(stdio("files")).unwrap();
        assert_eq!(management.test("files").unwrap(), health);
        assert_eq!(management.auth("files").unwrap(), health);
        let rows: Vec<McpStatusRow> = management.list_probed().unwrap();
        assert_eq!(rows[0].health, health);
        assert_eq!(
            management.list().unwrap()[0].health,
            McpHealth::Unknown,
            "a plain list measures nothing and says so"
        );
    }
}

#[test]
fn a_malformed_name_or_target_is_refused_before_anything_is_stored() {
    for bad in ["", "has space", "has/slash", &"x".repeat(65)] {
        assert_eq!(
            StoredServer::new(bad, McpTransportKind::Stdio, "/bin/x").unwrap_err(),
            McpManagementError::InvalidName,
            "accepted name {bad:?}"
        );
    }
    for bad in ["", "   ", "/bin/x\u{0}"] {
        assert!(matches!(
            StoredServer::new("ok", McpTransportKind::Stdio, bad).unwrap_err(),
            McpManagementError::InvalidTransport(_)
        ));
    }
    assert!(StoredServer::new("ok-name_1", McpTransportKind::Stdio, "/bin/x").is_ok());
}

/// A read-only operation must never write. A `list` that persists would rewrite
/// the user's settings file every time a panel refreshed.
#[test]
fn read_only_operations_never_write_to_the_store() {
    let store = Arc::new(MemoryStore::default());
    let management = McpManagement::new(store.clone() as Arc<dyn McpDefinitionStore>)
        .with_probe(Arc::new(Fixed(McpHealth::Reachable)));
    management.add(stdio("files")).unwrap();
    let writes_after_add = *store.writes.lock().unwrap();

    management.list().unwrap();
    management.auth("files").unwrap();
    management.test("files").unwrap();

    assert_eq!(*store.writes.lock().unwrap(), writes_after_add);
}

#[test]
fn management_plugin_publishes_the_settings_backed_effect_owned_service() {
    use heycode_core::{ContributionKind, Plugin, compose};
    use heycode_mcp::SERVICE_MCP_MANAGEMENT;
    use heycode_mcp::management::mcp_management_plugin;

    let plugin = mcp_management_plugin();
    assert_eq!(plugin.provides(), &[SERVICE_MCP_MANAGEMENT]);
    assert!(
        plugin
            .inject()
            .contains(&heycode_settings::SERVICE_SETTINGS)
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        plugin,
    ];
    let mut context = compose(&plugins).unwrap();
    let management = context
        .get::<McpManagement>(SERVICE_MCP_MANAGEMENT)
        .expect("the owning plugin must publish management");

    assert!(management.list().unwrap().is_empty());
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert_eq!(
        inventory
            .contributions
            .iter()
            .filter(|row| {
                row.kind == ContributionKind::Service && row.name == SERVICE_MCP_MANAGEMENT.as_str()
            })
            .map(|row| row.plugin)
            .collect::<Vec<_>>(),
        ["mcp-management"]
    );

    context.shutdown();
    assert_eq!(management.list().unwrap_err(), McpManagementError::Stopped);
}

#[test]
fn failed_later_composition_stops_a_held_management_service() {
    use heycode_core::{Context, CoreError, CoreResult, Plugin, PluginDescriptor, compose};
    use heycode_mcp::SERVICE_MCP_MANAGEMENT;
    use heycode_mcp::management::mcp_management_plugin;

    struct Capture(Arc<Mutex<Option<Arc<McpManagement>>>>);
    impl Plugin for Capture {
        fn name(&self) -> &'static str {
            "capture-management"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::unclassified(self.name())
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MCP_MANAGEMENT]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let management = context
                .get::<McpManagement>(SERVICE_MCP_MANAGEMENT)
                .ok_or_else(|| CoreError::other("management missing"))?;
            *self
                .0
                .lock()
                .map_err(|_| CoreError::other("capture unavailable"))? = Some(management);
            Ok(())
        }
    }

    struct Fails;
    impl Plugin for Fails {
        fn name(&self) -> &'static str {
            "fails-after-management"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::unclassified(self.name())
        }

        fn apply(&self, _context: &mut Context) -> CoreResult<()> {
            Err(CoreError::other("intentional failure"))
        }
    }

    let captured = Arc::new(Mutex::new(None));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        mcp_management_plugin(),
        Box::new(Capture(Arc::clone(&captured))),
        Box::new(Fails),
    ];

    assert!(compose(&plugins).is_err());
    let management = captured
        .lock()
        .unwrap()
        .clone()
        .expect("capture ran before the failure");
    assert_eq!(management.list().unwrap_err(), McpManagementError::Stopped);
}

mod settings_store {
    //! MCP10 persistence: layer precedence, and the administrator lock.

    use std::sync::{Arc, Mutex};

    use heycode_core::Context;
    use heycode_mcp::management::{
        McpDefinitionStore, McpManagement, McpManagementError, SettingsBackedStore,
        settings_definition, settings_namespace,
    };
    use heycode_mcp::{McpDefinitionScope, McpTransportKind};
    use heycode_settings::{SettingsDocuments, SettingsService, SettingsWriter};
    use serde_json::json;

    #[derive(Default)]
    struct CapturingWriter(Mutex<Vec<serde_json::Value>>);

    impl SettingsWriter for CapturingWriter {
        fn persist_user(
            &self,
            _namespace: &heycode_settings::SettingsNamespace,
            section: &serde_json::Value,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push(section.clone());
            Ok(())
        }
    }

    fn server(transport: &str, target: &str, enabled: bool) -> serde_json::Value {
        json!({"transport": transport, "target": target, "enabled": enabled})
    }

    fn store(
        user: Option<serde_json::Value>,
        managed: Option<serde_json::Value>,
    ) -> (Arc<CapturingWriter>, SettingsBackedStore, Context) {
        store_with_project(user, None, managed)
    }

    fn store_with_project(
        user: Option<serde_json::Value>,
        project: Option<serde_json::Value>,
        managed: Option<serde_json::Value>,
    ) -> (Arc<CapturingWriter>, SettingsBackedStore, Context) {
        let namespace = settings_namespace().unwrap();
        let mut documents = SettingsDocuments::new();
        if let Some(user) = user {
            documents.set_user(namespace.clone(), user).unwrap();
        }
        if let Some(project) = project {
            documents.set_project(namespace.clone(), project).unwrap();
        }
        if let Some(managed) = managed {
            documents.set_managed(namespace, managed).unwrap();
        }
        let writer = Arc::new(CapturingWriter::default());
        let service = Arc::new(SettingsService::with_writer(
            documents,
            writer.clone() as Arc<dyn SettingsWriter>,
        ));
        let context = Context::default();
        service
            .register(&context, settings_definition().unwrap())
            .unwrap();
        (writer, SettingsBackedStore::new(service), context)
    }

    #[test]
    fn definitions_round_trip_through_the_user_layer() {
        let (writer, store, _context) = store(
            Some(json!({"servers": {"files": server("stdio", "/bin/files", true)}})),
            None,
        );

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["files"].target, "/bin/files");
        assert_eq!(loaded["files"].transport, McpTransportKind::Stdio);
        assert_eq!(loaded["files"].scope, McpDefinitionScope::User);
        assert!(!loaded["files"].managed);

        store.persist(&loaded).unwrap();
        let written = writer.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(written["servers"]["files"]["target"], "/bin/files");
        assert_eq!(written["servers"]["files"]["transport"], "stdio");
    }

    /// An administrator definition wins over a user one of the same name, and
    /// carries its lock with it.
    #[test]
    fn a_managed_definition_wins_and_arrives_locked() {
        let (_writer, store, _context) = store(
            Some(json!({"servers": {"corp": server("stdio", "/bin/user-version", true)}})),
            Some(json!({"servers": {"corp": server("stdio", "/bin/admin-version", true)}})),
        );

        let loaded = store.load().unwrap();
        assert_eq!(loaded["corp"].target, "/bin/admin-version");
        assert!(loaded["corp"].managed);
        assert_eq!(loaded["corp"].scope, McpDefinitionScope::Managed);
    }

    /// The security property. Persisting must never copy an administrator
    /// definition down into the user layer, where the user could then edit it
    /// and escape the lock.
    #[test]
    fn persisting_never_writes_a_managed_definition_into_the_user_layer() {
        let (writer, store, _context) = store(
            Some(json!({"servers": {"mine": server("stdio", "/bin/mine", true)}})),
            Some(json!({"servers": {"corp": server("stdio", "/bin/admin", true)}})),
        );

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        store.persist(&loaded).unwrap();

        let written = writer.0.lock().unwrap().last().cloned().unwrap();
        let servers = written["servers"].as_object().unwrap();
        assert!(servers.contains_key("mine"));
        assert!(
            !servers.contains_key("corp"),
            "a managed definition must not be copied into the user layer: {written}"
        );
    }

    #[test]
    fn unrelated_user_change_preserves_a_project_shadowed_user_row_exactly() {
        let shadowed_user = json!({
            "transport": "stdio",
            "target": "/bin/user-shadow",
            "args": ["--user-owned"],
            "env": {"SOURCE": "user"},
            "enabled": false,
            "provenance_fixture": {"owner": "user", "keep": true}
        });
        let user = json!({
            "servers": {
                "mine": server("stdio", "/bin/mine", true),
                "shadowed": shadowed_user.clone()
            },
            "unrelated_user_fixture": {"keep": "exactly"}
        });
        let project = json!({
            "servers": {
                "project_only": server("stdio", "/bin/project-only", true),
                "shadowed": server("stdio", "/bin/project-shadow", true)
            }
        });
        let (writer, store, _context) =
            store_with_project(Some(user.clone()), Some(project.clone()), None);
        let management = McpManagement::new(Arc::new(store));

        let before = management.list().unwrap();
        let shadowed = before
            .iter()
            .find(|row| row.server.name == "shadowed")
            .unwrap();
        assert_eq!(shadowed.server.scope, McpDefinitionScope::Project);
        assert_eq!(shadowed.server.target, "/bin/project-shadow");

        management.enable("mine", false).unwrap();
        let written = writer.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(written["servers"]["shadowed"], shadowed_user);
        assert_eq!(
            written["unrelated_user_fixture"],
            user["unrelated_user_fixture"]
        );
        assert_eq!(written["servers"]["mine"]["enabled"], false);
        assert!(written["servers"].get("project_only").is_none());
        assert_eq!(
            management
                .list()
                .unwrap()
                .iter()
                .find(|row| row.server.name == "shadowed")
                .unwrap()
                .server
                .scope,
            McpDefinitionScope::Project
        );

        let writes = writer.0.lock().unwrap().len();
        for result in [
            management.enable("shadowed", false),
            management.edit("shadowed", McpTransportKind::Stdio, "/bin/replaced"),
            management.remove("shadowed"),
        ] {
            assert_eq!(
                result,
                Err(McpManagementError::ReadOnlyScope {
                    name: "shadowed".to_owned(),
                    scope: McpDefinitionScope::Project,
                })
            );
        }
        assert_eq!(writer.0.lock().unwrap().len(), writes);
        assert_eq!(written["servers"]["shadowed"]["target"], "/bin/user-shadow");
        assert_eq!(
            project["servers"]["shadowed"]["target"],
            "/bin/project-shadow"
        );
    }

    #[test]
    fn settings_update_uses_one_exact_snapshot_and_never_retries_a_stale_mutation() {
        let namespace = settings_namespace().unwrap();
        let mut documents = SettingsDocuments::new();
        documents
            .set_user(
                namespace.clone(),
                json!({"servers": {"mine": server("stdio", "/bin/original", true)}}),
            )
            .unwrap();
        let writer = Arc::new(CapturingWriter::default());
        let service = Arc::new(SettingsService::with_writer(
            documents,
            writer.clone() as Arc<dyn SettingsWriter>,
        ));
        let context = Context::default();
        service
            .register(&context, settings_definition().unwrap())
            .unwrap();
        let store = SettingsBackedStore::new(service.clone());

        let concurrent = service.clone();
        let concurrent_namespace = namespace.clone();
        let mut mutation = move |servers: &mut std::collections::BTreeMap<
            String,
            heycode_mcp::management::StoredServer,
        >| {
            concurrent
                .replace_user(
                    &concurrent_namespace,
                    json!({"servers": {"mine": server("stdio", "/bin/concurrent", true)}}),
                    None,
                )
                .unwrap();
            servers.get_mut("mine").unwrap().enabled = false;
            Ok(())
        };
        let error = store.update(&mut mutation).unwrap_err();
        assert!(
            error.to_string().contains("changed since its snapshot"),
            "{error}"
        );

        let writes = writer.0.lock().unwrap();
        assert_eq!(writes.len(), 1, "the stale candidate must not be retried");
        assert_eq!(writes[0]["servers"]["mine"]["target"], "/bin/concurrent");
        assert_eq!(writes[0]["servers"]["mine"]["enabled"], true);
    }

    /// The schema refuses a malformed section outright and names the offending
    /// entry. Failing loud beats silently dropping a server the user believes
    /// they configured — they would never find out why it is missing.
    #[test]
    fn a_malformed_entry_is_refused_by_the_schema_and_names_itself() {
        let namespace = settings_namespace().unwrap();
        for (bad, needle) in [
            (
                json!({"servers": {"bad name": server("stdio", "/bin/x", true)}}),
                "bad name",
            ),
            (
                json!({"servers": {"srv": {"transport": "carrier-pigeon", "target": "/bin/x"}}}),
                "carrier-pigeon",
            ),
            (
                json!({"servers": {"srv": {"target": "/bin/x"}}}),
                "transport",
            ),
            (
                json!({"servers": {"srv": {"transport": "stdio"}}}),
                "target",
            ),
            (
                json!({"servers": {"srv": server("stdio", "", true)}}),
                "empty target",
            ),
        ] {
            let mut documents = SettingsDocuments::new();
            documents.set_user(namespace.clone(), bad.clone()).unwrap();
            let service = Arc::new(SettingsService::with_writer(
                documents,
                Arc::new(CapturingWriter::default()) as Arc<dyn SettingsWriter>,
            ));
            let context = Context::default();
            let error = service
                .register(&context, settings_definition().unwrap())
                .expect_err("a malformed section must be refused");
            assert!(
                error.to_string().contains(needle),
                "for {bad}, error must name `{needle}`: {error}"
            );
        }
    }

    /// Defense in depth, and a live branch rather than decoration: the schema
    /// checks that a target is non-empty but not that it is free of control
    /// characters, which `StoredServer` does refuse. Such a row is skipped so
    /// one bad entry cannot hide every other server.
    #[test]
    fn an_entry_the_schema_allows_but_the_model_rejects_is_skipped() {
        let (_writer, store, _context) = store(
            Some(json!({"servers": {
                "good": server("stdio", "/bin/good", true),
                "sneaky": server("stdio", "/bin/x\u{0}", true),
            }})),
            None,
        );
        assert_eq!(
            store.load().unwrap().keys().collect::<Vec<_>>(),
            vec!["good"]
        );
    }

    /// An entry with no `enabled` key is enabled — absent means default, and
    /// the default for a server someone deliberately added is on.
    #[test]
    fn an_entry_without_an_enabled_flag_defaults_to_enabled() {
        let (_writer, store, _context) = store(
            Some(json!({"servers": {"files": {"transport": "stdio", "target": "/bin/f"}}})),
            None,
        );
        assert!(store.load().unwrap()["files"].enabled);
    }

    #[test]
    fn an_unregistered_or_empty_namespace_loads_as_no_servers() {
        let (_writer, store, _context) = store(None, None);
        assert!(store.load().unwrap().is_empty());
    }

    /// End to end through the operations layer, on the real settings stack.
    #[test]
    fn management_over_the_settings_store_refuses_to_edit_a_managed_server() {
        let (_writer, store, _context) = store(
            None,
            Some(json!({"servers": {"corp": server("stdio", "/bin/admin", true)}})),
        );
        let management = McpManagement::new(Arc::new(store) as Arc<dyn McpDefinitionStore>);

        assert_eq!(
            management
                .edit("corp", McpTransportKind::Stdio, "/bin/evil")
                .unwrap_err(),
            McpManagementError::Managed("corp".to_owned())
        );
        assert_eq!(management.list().unwrap()[0].server.target, "/bin/admin");
    }
}

/// The overlay is what makes the two surfaces one. A configured server is
/// listed, is not connectable *through the store* (the caller already owns it),
/// and refuses every mutating operation by naming the file that owns it.
#[test]
fn configured_servers_are_listed_read_only_and_never_persisted() {
    let (store, management) = management();
    management.add(stdio("stored")).unwrap();
    management
        .adopt_configured_servers(BTreeMap::from([(
            "declared".to_owned(),
            StoredServer::new("declared", McpTransportKind::Stdio, "/usr/bin/declared").unwrap(),
        )]))
        .unwrap();

    let listed: Vec<String> = management
        .list()
        .unwrap()
        .into_iter()
        .map(|row| row.server.name)
        .collect();
    assert_eq!(listed, ["declared".to_owned(), "stored".to_owned()]);

    // Only the persisted row is the store's to hand to a connection provider.
    let connectable: Vec<String> = management
        .connectable()
        .unwrap()
        .into_iter()
        .map(|server| server.name)
        .collect();
    assert_eq!(connectable, ["stored".to_owned()]);

    for outcome in [
        management.remove("declared"),
        management.enable("declared", false),
        management.edit("declared", McpTransportKind::Stdio, "/usr/bin/other"),
        management.add(stdio("declared")),
    ] {
        assert_eq!(
            outcome,
            Err(McpManagementError::ConfigDeclared("declared".to_owned()))
        );
    }
    assert!(!store.servers.lock().unwrap().contains_key("declared"));
}

/// A disabled row is not a connection, and a configured name shadows a stored
/// one so the two sources can never produce two connections for one server.
#[test]
fn connectable_skips_disabled_rows_and_names_configuration_already_owns() {
    let (_store, management) = management();
    management.add(stdio("on")).unwrap();
    management.add(stdio("off")).unwrap();
    management.add(stdio("shadowed")).unwrap();
    management.enable("off", false).unwrap();
    management
        .adopt_configured_servers(BTreeMap::from([(
            "shadowed".to_owned(),
            StoredServer::new("shadowed", McpTransportKind::Stdio, "/usr/bin/exact").unwrap(),
        )]))
        .unwrap();

    let connectable: Vec<String> = management
        .connectable()
        .unwrap()
        .into_iter()
        .map(|server| server.name)
        .collect();
    assert_eq!(connectable, ["on".to_owned()]);

    // The configured transport is the one the surface reports for that name.
    let row = management
        .list()
        .unwrap()
        .into_iter()
        .find(|row| row.server.name == "shadowed")
        .unwrap();
    assert_eq!(row.server.target, "/usr/bin/exact");
}

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn the_live_probe_reports_reachable_unreachable_and_authorization_in_bounded_time() {
    use heycode_mcp::management::LiveMcpProbe;
    let probe = LiveMcpProbe::with_timeout(std::time::Duration::from_secs(20));

    // A command that does not exist: unreachable, quickly, no hang.
    let missing = StoredServer::new(
        "missing",
        McpTransportKind::Stdio,
        "/nonexistent/heycode-probe-target",
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(probe.probe(&missing), McpHealth::Unreachable);
    assert!(started.elapsed() < std::time::Duration::from_secs(15));

    // A real stdio server answers initialize and is torn down again.
    if python3_available() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp15_server.py");
        let live = StoredServer::new("live", McpTransportKind::Stdio, "python3")
            .unwrap()
            .with_stdio_launch(
                vec![fixture.display().to_string(), "stdio".to_owned()],
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(probe.probe(&live), McpHealth::Reachable);
    }

    // HTTP: a closed port is unreachable; a 401 needs authorization.
    let closed = StoredServer::new(
        "closed",
        McpTransportKind::StreamableHttp,
        "http://127.0.0.1:9/mcp",
    )
    .unwrap();
    assert_eq!(probe.probe(&closed), McpHealth::Unreachable);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        if let Ok((mut socket, _)) = listener.accept() {
            let mut request = vec![0_u8; 8192];
            let _ = socket.read(&mut request);
            let _ = socket.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let locked = StoredServer::new(
        "locked",
        McpTransportKind::StreamableHttp,
        format!("http://{address}/mcp"),
    )
    .unwrap();
    assert_eq!(probe.probe(&locked), McpHealth::AuthorizationRequired);

    // `test` through management now has a real answer.
    let store = Arc::new(MemoryStore::default());
    store
        .servers
        .lock()
        .unwrap()
        .insert("missing".to_owned(), missing);
    let management =
        McpManagement::new(store as Arc<dyn McpDefinitionStore>).with_probe(Arc::new(probe));
    assert_eq!(management.test("missing").unwrap(), McpHealth::Unreachable);
}

/// Opening a surface must not start every configured server. Probing is
/// something a user asks for: `test` for one, `list_probed` for all.
#[test]
fn listing_never_probes_and_probing_many_runs_concurrently_in_input_order() {
    #[derive(Default)]
    struct CountingProbe {
        calls: Mutex<Vec<String>>,
        in_flight: Mutex<usize>,
        peak: Mutex<usize>,
    }
    impl McpProbe for CountingProbe {
        fn probe(&self, server: &StoredServer) -> McpHealth {
            self.calls.lock().unwrap().push(server.name.clone());
            {
                let mut in_flight = self.in_flight.lock().unwrap();
                *in_flight += 1;
                let mut peak = self.peak.lock().unwrap();
                *peak = (*peak).max(*in_flight);
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
            *self.in_flight.lock().unwrap() -= 1;
            if server.name == "b" {
                McpHealth::Unreachable
            } else {
                McpHealth::Reachable
            }
        }
    }

    let store = Arc::new(MemoryStore::default());
    for name in ["a", "b", "c", "d"] {
        store
            .servers
            .lock()
            .unwrap()
            .insert(name.to_owned(), stdio(name));
    }
    let probe = Arc::new(CountingProbe::default());
    let management = McpManagement::new(store as Arc<dyn McpDefinitionStore>)
        .with_probe(probe.clone() as Arc<dyn McpProbe>);

    let rows = management.list().unwrap();
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter().all(|row| row.health == McpHealth::Unknown),
        "list is a store read: it states no health it did not measure"
    );
    assert!(
        probe.calls.lock().unwrap().is_empty(),
        "and it probes nothing"
    );

    let started = std::time::Instant::now();
    let probed = management.list_probed().unwrap();
    let elapsed = started.elapsed();
    assert_eq!(
        probed
            .iter()
            .map(|row| (row.server.name.as_str(), row.health))
            .collect::<Vec<_>>(),
        [
            ("a", McpHealth::Reachable),
            ("b", McpHealth::Unreachable),
            ("c", McpHealth::Reachable),
            ("d", McpHealth::Reachable),
        ],
        "results keep definition order however they were scheduled"
    );
    assert!(*probe.peak.lock().unwrap() > 1, "probes overlap");
    assert!(
        elapsed < std::time::Duration::from_millis(280),
        "four 80ms probes in parallel, not 320ms serial: {elapsed:?}"
    );

    // One explicit test still probes exactly one server.
    probe.calls.lock().unwrap().clear();
    assert_eq!(management.test("b").unwrap(), McpHealth::Unreachable);
    assert_eq!(probe.calls.lock().unwrap().as_slice(), ["b"]);
}

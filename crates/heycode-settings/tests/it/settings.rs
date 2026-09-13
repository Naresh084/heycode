//! Layer resolution, immutable snapshot, validation, and plugin contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{PluginSource, compose};
use heycode_settings::{
    SERVICE_SETTINGS, SettingsApplies, SettingsDefinition, SettingsDocuments, SettingsError,
    SettingsNamespace, SettingsSchema, SettingsService, settings_plugin,
};
use serde_json::json;

fn agent_schema() -> SettingsSchema {
    SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {
                "model": {"type": "string"},
                "nested": {"type": "object"},
                "list": {"type": "array"}
            }
        }),
        json!({
            "model": "default-model",
            "nested": {"from_default": true, "winner": "default"},
            "list": ["default"]
        }),
        |value| {
            value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .filter(|model| !model.is_empty())
                .map(|_| ())
                .ok_or_else(|| "model must be a non-empty string".to_owned())
        },
    )
    .unwrap()
}

#[test]
fn session_override_is_validated_not_persisted_and_lost_on_recomposition() {
    struct RefuseWriter;
    impl heycode_settings::SettingsWriter for RefuseWriter {
        fn persist_user(&self, _: &SettingsNamespace, _: &serde_json::Value) -> Result<(), String> {
            Err("override must never invoke persistence".into())
        }
    }
    let namespace = SettingsNamespace::new("session-override").unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(namespace.clone(), json!({"model":"saved-model"}))
        .unwrap();
    let service =
        SettingsService::with_writer(documents.clone(), std::sync::Arc::new(RefuseWriter));
    let context = heycode_core::Context::new();
    service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&context, &namespace, move |change| {
            sender.send(change.source()).unwrap();
        })
        .unwrap();
    let changed = service
        .replace_override(&namespace, json!({"model":"session-model"}), Some(0))
        .unwrap();
    assert_eq!(changed.user().unwrap()["model"], "saved-model");
    assert_eq!(changed.resolved()["model"], "session-model");
    assert_eq!(
        service
            .get_without_override(&namespace)
            .unwrap()
            .unwrap()
            .resolved()["model"],
        "saved-model"
    );
    assert_eq!(
        receiver.try_recv().unwrap(),
        heycode_settings::SettingsUpdateSource::OverrideWrite
    );
    assert!(matches!(
        service.replace_override(&namespace, json!({"model":"stale"}), Some(0)),
        Err(SettingsError::Conflict { .. })
    ));
    assert!(
        service
            .replace_override(&namespace, json!({"model":""}), Some(1))
            .is_err()
    );
    assert_eq!(service.get(&namespace).unwrap().unwrap().revision(), 1);
    assert!(receiver.try_recv().is_err());
    let fresh = SettingsService::new(documents);
    let restored = fresh
        .register(&context, SettingsDefinition::new(namespace, agent_schema()))
        .unwrap();
    assert_eq!(restored.resolved()["model"], "saved-model");
    assert!(restored.override_layer().is_none());
}

#[test]
fn defaults_base_user_and_project_resolve_recursively_in_precedence_order() {
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            namespace.clone(),
            json!({
                "nested": {"winner": "user", "from_user": true},
                "list": ["user"]
            }),
        )
        .unwrap();
    documents
        .set_project(namespace.clone(), json!({"model": "project-model"}))
        .unwrap();
    let service = SettingsService::new(documents);
    let mut lifecycle = heycode_core::Context::new();
    let definition = SettingsDefinition::new(namespace.clone(), agent_schema())
        .with_base(json!({
            "model": "base-model",
            "nested": {"winner": "base", "from_base": true}
        }))
        .unwrap()
        .with_applies(SettingsApplies::Restart);

    let snapshot = service.register(&lifecycle, definition).unwrap();

    assert_eq!(
        snapshot.resolved(),
        &json!({
            "model": "project-model",
            "nested": {
                "from_default": true,
                "winner": "user",
                "from_base": true,
                "from_user": true
            },
            "list": ["user"]
        })
    );
    assert_eq!(snapshot.defaults()["model"], "default-model");
    assert_eq!(snapshot.base().unwrap()["model"], "base-model");
    assert_eq!(snapshot.user().unwrap()["nested"]["winner"], "user");
    assert_eq!(snapshot.project().unwrap()["model"], "project-model");
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    assert_eq!(snapshot.namespace(), &namespace);
    assert_eq!(snapshot.schema()["type"], "object");

    let mut detached = snapshot.resolved().clone();
    detached["model"] = json!("mutated-copy");
    assert_eq!(snapshot.resolved()["model"], "project-model");
    assert_eq!(
        service.get(&namespace).unwrap().unwrap().resolved()["model"],
        "project-model",
        "callers cannot mutate the frozen service snapshot"
    );

    lifecycle.shutdown();
    assert!(service.get(&namespace).unwrap().is_none());
}

#[test]
fn wire_exposure_is_explicit_and_defaults_to_fail_closed() {
    let service = SettingsService::new(SettingsDocuments::new());
    let mut lifecycle = heycode_core::Context::new();
    let private_namespace = SettingsNamespace::new("private-owner").unwrap();
    let public_namespace = SettingsNamespace::new("public-owner").unwrap();

    let private = service
        .register(
            &lifecycle,
            SettingsDefinition::new(private_namespace, agent_schema()),
        )
        .unwrap();
    let public = service
        .register(
            &lifecycle,
            SettingsDefinition::new(public_namespace, agent_schema().with_wire_exposure()),
        )
        .unwrap();

    assert!(!private.wire_exposed());
    assert!(public.wire_exposed());
    lifecycle.shutdown();
}

#[test]
fn invalid_names_layers_values_and_duplicate_registrations_fail_loud() {
    assert!(SettingsNamespace::new("Bad Namespace").is_err());

    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let mut documents = SettingsDocuments::new();
    let layer_error = documents
        .set_user(namespace.clone(), json!(["not", "an", "object"]))
        .unwrap_err();
    assert!(matches!(
        layer_error,
        SettingsError::LayerMustBeObject { .. }
    ));

    let service = SettingsService::new(SettingsDocuments::new());
    let mut lifecycle = heycode_core::Context::new();
    let first = service
        .register(
            &lifecycle,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap();
    let duplicate = service
        .register(
            &lifecycle,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap_err();
    assert!(matches!(
        duplicate,
        SettingsError::DuplicateNamespace { .. }
    ));
    drop(first);
    lifecycle.shutdown();

    let invalid = SettingsSchema::new(json!({"type": "object"}), json!({"model": ""}), |_value| {
        Err("cross-field validation failed".to_owned())
    })
    .unwrap_err();
    assert!(
        invalid
            .to_string()
            .contains("cross-field validation failed")
    );
}

#[test]
fn settings_plugin_publishes_a_described_service() {
    let plugins = vec![settings_plugin(SettingsDocuments::new())];
    let context = compose(&plugins).unwrap();

    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    assert!(settings.describe().unwrap().is_empty());
    let descriptor = &context.plugin_descriptors()[0];
    assert_eq!(descriptor.id, "settings");
    assert_eq!(descriptor.source, PluginSource::BuiltIn);
    assert_eq!(
        descriptor.contributions,
        &[
            heycode_core::PluginContributionKind::Service,
            heycode_core::PluginContributionKind::Provider,
        ]
    );
}

#[test]
fn provider_failure_never_publishes_the_candidate_snapshot() {
    struct FailingWriter;
    impl heycode_settings::SettingsWriter for FailingWriter {
        fn persist_user(
            &self,
            _namespace: &SettingsNamespace,
            _section: &serde_json::Value,
        ) -> Result<(), String> {
            Err("disk refused the write".to_owned())
        }
    }

    let service =
        SettingsService::with_writer(SettingsDocuments::new(), std::sync::Arc::new(FailingWriter));
    let mut context = heycode_core::Context::new();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap();

    let error = service
        .replace_user(&namespace, json!({"model": "candidate"}), Some(0))
        .unwrap_err();
    assert!(matches!(error, SettingsError::Provider { .. }));
    assert_eq!(
        service.get(&namespace).unwrap().unwrap().resolved()["model"],
        "default-model",
        "publish must happen only after durable persistence succeeds"
    );
    context.shutdown();
}

#[test]
fn stale_expected_revision_conflicts_before_persistence_or_publication() {
    #[derive(Default)]
    struct CountingWriter(std::sync::atomic::AtomicUsize);
    impl heycode_settings::SettingsWriter for CountingWriter {
        fn persist_user(
            &self,
            _namespace: &SettingsNamespace,
            _section: &serde_json::Value,
        ) -> Result<(), String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    let writer = std::sync::Arc::new(CountingWriter::default());
    let service = SettingsService::with_writer(SettingsDocuments::new(), writer.clone());
    let mut owner_context = heycode_core::Context::new();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let initial = service
        .register(
            &owner_context,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap();
    assert_eq!(initial.revision(), 0);

    let committed = service
        .replace_user(&namespace, json!({"model": "first"}), Some(0))
        .unwrap();
    assert_eq!(committed.revision(), 1);
    let error = service
        .replace_user(&namespace, json!({"model": "stale"}), Some(0))
        .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::Conflict {
            expected: 0,
            actual: 1,
            ..
        }
    ));
    assert_eq!(writer.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        service.get(&namespace).unwrap().unwrap().resolved()["model"],
        "first"
    );
    owner_context.shutdown();
}

#[test]
fn watchers_are_commit_ordered_panic_contained_and_context_disposed() {
    struct SuccessWriter;
    impl heycode_settings::SettingsWriter for SuccessWriter {
        fn persist_user(
            &self,
            _namespace: &SettingsNamespace,
            _section: &serde_json::Value,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    let service =
        SettingsService::with_writer(SettingsDocuments::new(), std::sync::Arc::new(SuccessWriter));
    let mut owner_context = heycode_core::Context::new();
    let mut watch_context = heycode_core::Context::new();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    service
        .register(
            &owner_context,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap();
    service
        .watch(&watch_context, &namespace, |_change| panic!("contained"))
        .unwrap();
    let revisions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = revisions.clone();
    service
        .watch(&watch_context, &namespace, move |change| {
            observed.lock().unwrap().push((
                change.previous().revision(),
                change.next().revision(),
                change.source(),
            ));
        })
        .unwrap();
    let reentrant_errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_errors = reentrant_errors.clone();
    let reentrant_service = service.clone();
    let reentrant_namespace = namespace.clone();
    service
        .watch(&watch_context, &namespace, move |change| {
            let error = reentrant_service
                .replace_user(
                    &reentrant_namespace,
                    json!({"model": "recursive"}),
                    Some(change.next().revision()),
                )
                .unwrap_err();
            captured_errors.lock().unwrap().push(error);
        })
        .unwrap();

    service
        .replace_user(&namespace, json!({"model": "one"}), Some(0))
        .unwrap();
    service
        .replace_user(&namespace, json!({"model": "two"}), Some(1))
        .unwrap();
    assert_eq!(
        *revisions.lock().unwrap(),
        vec![
            (0, 1, heycode_settings::SettingsUpdateSource::UserWrite),
            (1, 2, heycode_settings::SettingsUpdateSource::UserWrite),
        ]
    );
    assert!(
        reentrant_errors
            .lock()
            .unwrap()
            .iter()
            .all(|error| matches!(error, SettingsError::ReentrantWrite))
    );

    watch_context.shutdown();
    service
        .replace_user(&namespace, json!({"model": "three"}), Some(2))
        .unwrap();
    assert_eq!(revisions.lock().unwrap().len(), 2);
    owner_context.shutdown();
}

#[test]
fn provider_reload_is_atomic_and_keeps_last_good_generation_on_validation_failure() {
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let mut initial_documents = SettingsDocuments::new();
    initial_documents
        .set_user(namespace.clone(), json!({"model": "initial"}))
        .unwrap();
    let service = SettingsService::new(initial_documents);
    let mut owner_context = heycode_core::Context::new();
    let mut watch_context = heycode_core::Context::new();
    service
        .register(
            &owner_context,
            SettingsDefinition::new(namespace.clone(), agent_schema()),
        )
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&watch_context, &namespace, move |change| {
            let _ = sender.send(change);
        })
        .unwrap();

    let mut valid = SettingsDocuments::new();
    valid
        .set_user(namespace.clone(), json!({"model": "external"}))
        .unwrap();
    service.publish_documents(valid).unwrap();
    let change = receiver.recv().unwrap();
    assert_eq!(
        change.source(),
        heycode_settings::SettingsUpdateSource::ProviderReload
    );
    assert_eq!(change.next().revision(), 1);

    let mut invalid = SettingsDocuments::new();
    invalid
        .set_user(namespace.clone(), json!({"model": 7}))
        .unwrap();
    assert!(service.publish_documents(invalid).is_err());
    let current = service.get(&namespace).unwrap().unwrap();
    assert_eq!(current.revision(), 1);
    assert_eq!(current.resolved()["model"], "external");
    assert!(
        receiver.try_recv().is_err(),
        "invalid reload must not notify"
    );
    watch_context.shutdown();
    owner_context.shutdown();
}

#[test]
fn a_command_line_override_beats_user_and_project_until_the_user_changes_the_setting() {
    struct OkWriter;
    impl heycode_settings::SettingsWriter for OkWriter {
        fn persist_user(
            &self,
            _namespace: &SettingsNamespace,
            _section: &serde_json::Value,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(namespace.clone(), json!({"model": "user-model"}))
        .unwrap();
    documents
        .set_project(namespace.clone(), json!({"nested": {"winner": "project"}}))
        .unwrap();
    let service = SettingsService::with_writer(documents, std::sync::Arc::new(OkWriter));
    let mut context = heycode_core::Context::new();
    let definition = SettingsDefinition::new(namespace.clone(), agent_schema())
        .with_base(json!({"model": "base-model"}))
        .unwrap()
        .with_override(json!({"model": "flag-model"}))
        .unwrap();

    let snapshot = service.register(&context, definition).unwrap();
    assert_eq!(
        snapshot.resolved()["model"],
        "flag-model",
        "an explicit command-line flag is the top ephemeral layer below managed"
    );
    assert_eq!(snapshot.override_layer().unwrap()["model"], "flag-model");
    assert_eq!(
        snapshot.user().unwrap()["model"],
        "user-model",
        "the overridden layers stay visible so the UI can say what was overridden"
    );
    assert_eq!(snapshot.resolved()["nested"]["winner"], "project");
    assert_eq!(
        heycode_settings::SettingsLayer::Override.to_string(),
        "process override"
    );

    let next = service
        .replace_user(&namespace, json!({"model": "chosen-in-session"}), None)
        .unwrap();
    assert_eq!(
        next.resolved()["model"],
        "chosen-in-session",
        "changing the setting in-session supersedes the flag instead of being silently ignored"
    );
    assert!(next.override_layer().is_none());

    let rejected = SettingsDefinition::new(namespace.clone(), agent_schema())
        .with_override(json!("not-an-object"));
    assert!(matches!(
        rejected,
        Err(SettingsError::LayerMustBeObject {
            layer: heycode_settings::SettingsLayer::Override,
            ..
        })
    ));
    context.shutdown();
}

#[test]
fn automatic_update_preserves_overrides_and_rejects_reload_and_invalid_candidate() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Writer(Arc<AtomicUsize>);
    impl heycode_settings::SettingsWriter for Writer {
        fn persist_user(&self, _: &SettingsNamespace, _: &serde_json::Value) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let writes = Arc::new(AtomicUsize::new(0));
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let service =
        SettingsService::with_writer(SettingsDocuments::new(), Arc::new(Writer(writes.clone())));
    let mut context = heycode_core::Context::new();
    let original = service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), agent_schema())
                .with_override(json!({"nested": {"winner": "flag"}}))
                .unwrap(),
        )
        .unwrap();
    let next = service
        .replace_user_automatically(
            &namespace,
            json!({"model": "fallback"}),
            &original,
            |candidate| {
                assert_eq!(candidate.resolved()["model"], "fallback");
                assert_eq!(candidate.resolved()["nested"]["winner"], "flag");
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(next.override_layer().unwrap()["nested"]["winner"], "flag");
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let rejected =
        service.replace_user_automatically(&namespace, json!({"model": "bad"}), &next, |_| {
            Err(SettingsError::InvalidResolved {
                namespace: namespace.to_string(),
                message: "candidate rejected".into(),
            })
        });
    assert!(matches!(
        rejected,
        Err(SettingsError::InvalidResolved { .. })
    ));
    assert!(Arc::ptr_eq(
        &next,
        &service.get(&namespace).unwrap().unwrap()
    ));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let mut reload = SettingsDocuments::new();
    reload
        .set_user(namespace.clone(), json!({"model": "fallback"}))
        .unwrap();
    reload
        .set_project(namespace.clone(), json!({"model": "project"}))
        .unwrap();
    service.publish_documents(reload).unwrap();
    let reloaded = service.get(&namespace).unwrap().unwrap();
    assert_eq!(
        next.revision(),
        reloaded.revision(),
        "project reload preserves the user revision"
    );
    let stale = service.replace_user_automatically(
        &namespace,
        json!({"model": "stale"}),
        &next,
        |_| Ok(()),
    );
    assert!(matches!(stale, Err(SettingsError::StaleSnapshot { .. })));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        service.get(&namespace).unwrap().unwrap().resolved()["model"],
        "project"
    );
    context.shutdown();
}

//! S15 administrator-managed layer: precedence, locks and reload.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use heycode_settings::{
    SettingsDefinition, SettingsDocuments, SettingsError, SettingsLayer, SettingsNamespace,
    SettingsSchema, SettingsService, SettingsUpdateSource, SettingsWriter,
};
use serde_json::{Value, json};

#[derive(Default)]
struct CountingWriter(AtomicUsize);

impl SettingsWriter for CountingWriter {
    fn persist_user(&self, _namespace: &SettingsNamespace, _section: &Value) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn namespace() -> SettingsNamespace {
    SettingsNamespace::new("agent-runtime").unwrap()
}

fn schema() -> SettingsSchema {
    SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {
                "model": {"type": "string"},
                "nested": {"type": "object"},
                "note": {"type": "string"}
            }
        }),
        json!({"model": "default-model", "nested": {"depth": 1}, "note": "default"}),
        |value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
                .map(|_| ())
                .ok_or_else(|| "model must be a non-empty string".to_owned())
        },
    )
    .unwrap()
}

#[test]
fn managed_layer_outranks_project_and_reports_its_locked_paths() {
    let owner = namespace();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            owner.clone(),
            json!({"model": "user-model", "note": "user"}),
        )
        .unwrap();
    documents
        .set_project(
            owner.clone(),
            json!({"model": "project-model", "nested": {"depth": 2}}),
        )
        .unwrap();
    documents
        .set_managed(
            owner.clone(),
            json!({"model": "managed-model", "nested": {"depth": 9}}),
        )
        .unwrap();
    assert_eq!(
        documents.managed_section(&owner).unwrap()["model"],
        "managed-model"
    );

    let service = SettingsService::new(documents);
    let mut context = heycode_core::Context::new();
    let snapshot = service
        .register(&context, SettingsDefinition::new(owner.clone(), schema()))
        .unwrap();

    assert_eq!(
        snapshot.resolved(),
        &json!({"model": "managed-model", "nested": {"depth": 9}, "note": "user"}),
        "managed is the final administrator constraint above project"
    );
    assert_eq!(snapshot.managed().unwrap()["model"], "managed-model");
    assert_eq!(
        snapshot.managed_locks().to_vec(),
        vec!["model".to_owned(), "nested.depth".to_owned()],
        "locks are the leaf paths the administrator assigned, in path order"
    );
    assert_eq!(SettingsLayer::Managed.to_string(), "managed");
    context.shutdown();
}

#[test]
fn user_write_to_a_managed_locked_path_fails_before_persistence_or_publication() {
    let owner = namespace();
    let mut documents = SettingsDocuments::new();
    documents
        .set_managed(owner.clone(), json!({"model": "managed-model"}))
        .unwrap();
    let writer = Arc::new(CountingWriter::default());
    let service = SettingsService::with_writer(documents, writer.clone());
    let mut context = heycode_core::Context::new();
    let mut watch_context = heycode_core::Context::new();
    service
        .register(&context, SettingsDefinition::new(owner.clone(), schema()))
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&watch_context, &owner, move |change| {
            let _ = sender.send(change);
        })
        .unwrap();

    let error = service
        .replace_user(&owner, json!({"model": "user-model"}), Some(0))
        .unwrap_err();

    assert!(matches!(
        service.replace_override(&owner, json!({"model":"session-model"}), Some(0)),
        Err(SettingsError::ManagedLock { .. })
    ));
    assert!(
        service
            .get(&owner)
            .unwrap()
            .unwrap()
            .override_layer()
            .is_none()
    );

    assert!(
        matches!(
            &error,
            SettingsError::ManagedLock { namespace, path }
                if namespace == "agent-runtime" && path == "model"
        ),
        "a write resolution would silently ignore must fail loud instead: {error:?}"
    );
    assert_eq!(
        writer.0.load(Ordering::SeqCst),
        0,
        "a locked write never reaches the durable writer"
    );
    let current = service.get(&owner).unwrap().unwrap();
    assert_eq!(current.revision(), 0);
    assert!(current.user().is_none());
    assert!(receiver.try_recv().is_err(), "nothing was committed");

    let nested_error = service
        .replace_user(&owner, json!({"model": {"deep": "value"}}), Some(0))
        .unwrap_err();
    assert!(
        matches!(nested_error, SettingsError::ManagedLock { .. }),
        "a user object covering a managed leaf is the same conflict"
    );

    watch_context.shutdown();
    context.shutdown();
}

#[test]
fn unlocked_paths_still_commit_under_revision_cas_beside_a_managed_layer() {
    let owner = namespace();
    let mut documents = SettingsDocuments::new();
    documents
        .set_managed(owner.clone(), json!({"model": "managed-model"}))
        .unwrap();
    let writer = Arc::new(CountingWriter::default());
    let service = SettingsService::with_writer(documents, writer.clone());
    let mut context = heycode_core::Context::new();
    service
        .register(&context, SettingsDefinition::new(owner.clone(), schema()))
        .unwrap();

    let committed = service
        .replace_user(&owner, json!({"note": "user-note"}), Some(0))
        .unwrap();

    assert_eq!(committed.revision(), 1);
    assert_eq!(writer.0.load(Ordering::SeqCst), 1);
    assert_eq!(committed.resolved()["note"], "user-note");
    assert_eq!(
        committed.resolved()["model"],
        "managed-model",
        "the administrator constraint survives an unrelated user write"
    );

    let stale = service
        .replace_user(&owner, json!({"note": "stale"}), Some(0))
        .unwrap_err();
    assert!(matches!(stale, SettingsError::Conflict { .. }));
    assert_eq!(writer.0.load(Ordering::SeqCst), 1);
    context.shutdown();
}

#[test]
fn managed_reload_publishes_once_and_keeps_the_last_good_generation_on_failure() {
    let owner = namespace();
    let service = SettingsService::new(SettingsDocuments::new());
    let mut context = heycode_core::Context::new();
    let mut watch_context = heycode_core::Context::new();
    service
        .register(&context, SettingsDefinition::new(owner.clone(), schema()))
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&watch_context, &owner, move |change| {
            let _ = sender.send(change);
        })
        .unwrap();

    let mut managed = SettingsDocuments::new();
    managed
        .set_managed(owner.clone(), json!({"model": "policy-model"}))
        .unwrap();
    service.publish_documents(managed).unwrap();

    let change = receiver.recv().unwrap();
    assert_eq!(change.source(), SettingsUpdateSource::ProviderReload);
    assert_eq!(change.next().resolved()["model"], "policy-model");
    assert_eq!(
        change.next().revision(),
        0,
        "a managed-only change leaves the raw user revision alone"
    );
    assert_eq!(
        change.next().managed_locks().to_vec(),
        vec!["model".to_owned()]
    );

    let mut invalid = SettingsDocuments::new();
    invalid
        .set_managed(owner.clone(), json!({"model": ""}))
        .unwrap();
    assert!(service.publish_documents(invalid).is_err());
    let current = service.get(&owner).unwrap().unwrap();
    assert_eq!(current.resolved()["model"], "policy-model");
    assert!(
        receiver.try_recv().is_err(),
        "an invalid administrator generation notifies nobody"
    );

    watch_context.shutdown();
    context.shutdown();
}

#[test]
fn managed_sections_must_be_objects() {
    let owner = namespace();
    let mut documents = SettingsDocuments::new();
    let error = documents
        .set_managed(owner, json!(["not", "an", "object"]))
        .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::LayerMustBeObject {
            layer: SettingsLayer::Managed,
            ..
        }
    ));
}

#[test]
fn a_managed_generation_that_changes_no_resolved_value_still_republishes_its_locks() {
    let owner = namespace();
    let mut documents = SettingsDocuments::new();
    documents
        .set_project(owner.clone(), json!({"model": "agreed-model"}))
        .unwrap();
    let writer = Arc::new(CountingWriter::default());
    let service = SettingsService::with_writer(documents.clone(), writer.clone());
    let mut context = heycode_core::Context::new();
    let initial = service
        .register(&context, SettingsDefinition::new(owner.clone(), schema()))
        .unwrap();
    assert!(initial.managed_locks().is_empty());

    let mut reloaded = documents;
    reloaded
        .set_managed(owner.clone(), json!({"model": "agreed-model"}))
        .unwrap();
    service.publish_documents(reloaded).unwrap();

    let current = service.get(&owner).unwrap().unwrap();
    assert_eq!(
        current.resolved()["model"],
        "agreed-model",
        "the administrator agreed with the project value, so nothing resolved differently"
    );
    assert_eq!(
        current.managed_locks().to_vec(),
        vec!["model".to_owned()],
        "a policy that changes no value still changes who owns the path"
    );
    let error = service
        .replace_user(&owner, json!({"model": "user-model"}), Some(0))
        .unwrap_err();
    assert!(matches!(error, SettingsError::ManagedLock { .. }));
    assert_eq!(writer.0.load(Ordering::SeqCst), 0);
    context.shutdown();
}

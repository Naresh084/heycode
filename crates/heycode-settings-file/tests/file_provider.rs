//! Atomic persistence, format preservation, and file-safety contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::compose;
use heycode_settings::{
    SERVICE_SETTINGS, SettingsDefinition, SettingsNamespace, SettingsSchema, SettingsService,
};
use heycode_settings_file::{FILE_SCHEMA_VERSION, FileSettingsConfig, file_settings_plugin};
use serde_json::json;

fn schema() -> SettingsSchema {
    SettingsSchema::new(
        json!({"type": "object"}),
        json!({"model": "default", "nested": {"keep": true}}),
        |value| {
            value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(|_| ())
                .ok_or_else(|| "model must be a string".to_owned())
        },
    )
    .unwrap()
}

#[test]
fn replace_is_atomic_and_preserves_comments_unknown_tables_and_other_namespaces() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    let original = format!(
        "# keep this header\nschema_version = {FILE_SCHEMA_VERSION}\n\n[unknown]\nkeep = \"yes\" # keep inline\n\n[settings.other]\nvalue = 7\n\n[settings.agent-runtime]\nmodel = \"old\"\n"
    );
    std::fs::write(&path, original).unwrap();

    let plugins = vec![file_settings_plugin(
        FileSettingsConfig::user(path.clone()).without_watch(),
    )];
    let mut context = compose(&plugins).unwrap();
    let service = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), schema()),
        )
        .unwrap();

    let next = service
        .replace_user(
            &namespace,
            json!({"model": "new", "nested": {"from_user": true}}),
            Some(0),
        )
        .unwrap();
    assert_eq!(next.resolved()["model"], "new");
    assert_eq!(
        service.get(&namespace).unwrap().unwrap().resolved()["model"],
        "new"
    );

    let persisted = std::fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("# keep this header"), "{persisted}");
    assert!(persisted.contains("[unknown]"), "{persisted}");
    assert!(
        persisted.contains("keep = \"yes\" # keep inline"),
        "{persisted}"
    );
    assert!(persisted.contains("[settings.other]"), "{persisted}");
    assert!(persisted.contains("value = 7"), "{persisted}");
    assert!(
        persisted.contains("[settings.agent-runtime]"),
        "{persisted}"
    );
    assert!(persisted.contains("model = \"new\""), "{persisted}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "settings file must be owner-only: {mode:o}"
        );
    }
    context.shutdown();
}

#[test]
fn absent_file_is_created_with_schema_and_owner_only_mode_on_first_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/settings.toml");
    let plugins = vec![file_settings_plugin(
        FileSettingsConfig::user(path.clone()).without_watch(),
    )];
    let mut context = compose(&plugins).unwrap();
    let service = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), schema()),
        )
        .unwrap();
    service
        .replace_user(&namespace, json!({"model": "created"}), Some(0))
        .unwrap();

    let persisted = std::fs::read_to_string(&path).unwrap();
    assert!(persisted.contains(&format!("schema_version = {FILE_SCHEMA_VERSION}")));
    assert!(persisted.contains("[settings.agent-runtime]"));
    assert!(persisted.contains("model = \"created\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    context.shutdown();
}

#[test]
fn newer_schema_and_symlink_targets_fail_before_service_publication() {
    let directory = tempfile::tempdir().unwrap();
    let future = directory.path().join("future.toml");
    std::fs::write(
        &future,
        format!("schema_version = {}\n", FILE_SCHEMA_VERSION + 1),
    )
    .unwrap();
    let error = match compose(&[file_settings_plugin(
        FileSettingsConfig::user(future).without_watch(),
    )]) {
        Ok(_) => panic!("future settings schema must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("newer settings schema"), "{error}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let target = directory.path().join("target.toml");
        let link = directory.path().join("settings-link.toml");
        std::fs::write(&target, "schema_version = 1\n").unwrap();
        symlink(&target, &link).unwrap();
        let error = match compose(&[file_settings_plugin(
            FileSettingsConfig::user(link).without_watch(),
        )]) {
            Ok(_) => panic!("symlink settings file must fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("symbolic link"), "{error}");
    }
}

#[test]
fn trusted_project_layer_stays_read_only_and_wins_over_user_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let user_path = directory.path().join("user.toml");
    let project_path = directory.path().join("project.toml");
    std::fs::write(
        &user_path,
        "schema_version = 1\n[settings.agent-runtime]\nmodel = \"user\"\n",
    )
    .unwrap();
    let project_original = "schema_version = 1\n[settings.agent-runtime]\nmodel = \"project\"\n";
    std::fs::write(&project_path, project_original).unwrap();

    let config = FileSettingsConfig::user(user_path.clone())
        .with_project(project_path.clone())
        .without_watch();
    let plugins = vec![file_settings_plugin(config)];
    let mut context = compose(&plugins).unwrap();
    let service = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let initial = service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), schema()),
        )
        .unwrap();
    assert_eq!(initial.resolved()["model"], "project");

    let next = service
        .replace_user(&namespace, json!({"model": "new-user"}), Some(0))
        .unwrap();
    assert_eq!(next.resolved()["model"], "project");
    assert!(
        std::fs::read_to_string(&user_path)
            .unwrap()
            .contains("new-user")
    );
    assert_eq!(
        std::fs::read_to_string(&project_path).unwrap(),
        project_original,
        "the project layer is never a user-write target"
    );
    context.shutdown();
}

#[test]
fn external_atomic_replace_reloads_and_notifies_with_provider_source() {
    use std::io::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    std::fs::write(
        &path,
        "schema_version = 1\n[settings.agent-runtime]\nmodel = \"old\"\n",
    )
    .unwrap();
    let plugins = vec![file_settings_plugin(FileSettingsConfig::user(path.clone()))];
    let mut context = compose(&plugins).unwrap();
    let service = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let namespace = SettingsNamespace::new("agent-runtime").unwrap();
    let initial = service
        .register(
            &context,
            SettingsDefinition::new(namespace.clone(), schema()),
        )
        .unwrap();
    assert_eq!(initial.revision(), 0);
    assert_eq!(initial.resolved()["model"], "old");
    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&context, &namespace, move |change| {
            let _ = sender.send(change);
        })
        .unwrap();

    let mut output = atomic_write_file::AtomicWriteFile::open(&path).unwrap();
    output
        .write_all(b"schema_version = 1\n[settings.agent-runtime]\nmodel = \"external\"\n")
        .unwrap();
    output.commit().unwrap();

    let change = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("filesystem watcher did not publish external replacement");
    assert_eq!(
        change.source(),
        heycode_settings::SettingsUpdateSource::ProviderReload
    );
    assert_eq!(change.previous().revision(), 0);
    assert_eq!(change.next().revision(), 1);
    assert_eq!(change.next().resolved()["model"], "external");
    assert_eq!(
        service.get(&namespace).unwrap().unwrap().resolved()["model"],
        "external"
    );
    context.shutdown();
}

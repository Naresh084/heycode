//! CMD10 transactional theme and Vim-mode settings.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_core::Context;
use heycode_settings::{SettingsDocuments, SettingsNamespace, SettingsService, SettingsWriter};
use heycode_ui::preferences::{
    HeaderDensity, SettingsBackedUiPreferences, ShellChromePreferences, UiPreferences,
    settings_definition, settings_namespace,
};

#[derive(Default)]
struct RecordingWriter {
    stored: Mutex<Vec<serde_json::Value>>,
}

impl SettingsWriter for RecordingWriter {
    fn persist_user(
        &self,
        _namespace: &SettingsNamespace,
        section: &serde_json::Value,
    ) -> Result<(), String> {
        self.stored.lock().unwrap().push(section.clone());
        Ok(())
    }
}

fn service() -> (Arc<SettingsService>, Arc<RecordingWriter>, Context) {
    let writer = Arc::new(RecordingWriter::default());
    let service = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        writer.clone() as Arc<dyn SettingsWriter>,
    ));
    let context = Context::default();
    service
        .register(&context, settings_definition().unwrap())
        .unwrap();
    (service, writer, context)
}

#[test]
fn theme_and_vim_preferences_commit_with_exact_revision_and_reload() {
    let (settings, writer, mut context) = service();
    let store = SettingsBackedUiPreferences::new(settings.clone());
    let loaded = store.load().unwrap();
    assert_eq!(loaded.preferences.theme_id(), "heycode-dark");
    assert!(!loaded.preferences.vim_mode());
    assert!(!loaded.preferences.focus_view());
    assert_eq!(
        loaded.preferences.shell(),
        ShellChromePreferences::default()
    );

    let next = UiPreferences::new("heycode-high-contrast", true)
        .unwrap()
        .with_shell(
            ShellChromePreferences::new(HeaderDensity::Compact, false, true).with_header_pet(true),
        );
    let committed = store.store(&next, loaded.revision).unwrap();
    assert_eq!(committed.preferences, next);
    assert!(committed.revision > loaded.revision);
    assert_eq!(store.load().unwrap().preferences, next);
    assert_eq!(writer.stored.lock().unwrap().len(), 1);
    context.shutdown();
}

#[test]
fn malformed_shell_preferences_fail_before_persistence_or_publication() {
    let (settings, writer, mut context) = service();
    let namespace = settings_namespace().unwrap();
    let revision = settings.get(&namespace).unwrap().unwrap().revision();

    let error = settings
        .replace_user(
            &namespace,
            serde_json::json!({
                "theme": "heycode-dark",
                "vim_mode": false,
                "header_density": "floating",
                "header_pet": false,
                "footer_status": true,
                "footer_hints": true
            }),
            Some(revision),
        )
        .expect_err("an open header value must not enter Settings");

    assert!(error.to_string().contains("header density"), "{error}");
    assert!(writer.stored.lock().unwrap().is_empty());
    assert_eq!(
        SettingsBackedUiPreferences::new(settings)
            .load()
            .unwrap()
            .preferences
            .shell(),
        ShellChromePreferences::default()
    );
    context.shutdown();
}

#[test]
fn theme_and_vim_shortcuts_preserve_the_shell_preferences() {
    let (settings, writer, mut context) = service();
    let store = SettingsBackedUiPreferences::new(settings);
    let initial = store.load().unwrap();
    let shell =
        ShellChromePreferences::new(HeaderDensity::Compact, false, true).with_header_pet(true);
    let configured = store
        .store(
            &UiPreferences::new("heycode-dark", false)
                .unwrap()
                .with_shell(shell),
            initial.revision,
        )
        .unwrap();
    let themed = store
        .store_theme("heycode-high-contrast", configured.revision)
        .unwrap();
    assert_eq!(themed.preferences.shell(), shell);
    let vim = store.store_vim_mode(true, themed.revision).unwrap();
    assert_eq!(vim.preferences.shell(), shell);
    assert_eq!(vim.preferences.theme_id(), "heycode-high-contrast");
    assert!(vim.preferences.vim_mode());
    assert_eq!(writer.stored.lock().unwrap().len(), 3);
    context.shutdown();
}

#[test]
fn stale_ui_preference_write_is_a_conflict_not_an_overwrite() {
    let (settings, writer, mut context) = service();
    let store = SettingsBackedUiPreferences::new(settings);
    let first = store.load().unwrap();
    store
        .store(
            &UiPreferences::new("heycode-high-contrast", false).unwrap(),
            first.revision,
        )
        .unwrap();
    let error = store
        .store(
            &UiPreferences::new("heycode-dark", true).unwrap(),
            first.revision,
        )
        .expect_err("stale write must fail");
    assert!(error.to_string().contains("changed since"), "{error}");
    assert_eq!(writer.stored.lock().unwrap().len(), 1);
    context.shutdown();
}

#[test]
fn invalid_theme_ids_fail_before_the_settings_boundary() {
    assert!(UiPreferences::new("../theme", false).is_err());
    assert_eq!(settings_namespace().unwrap().as_str(), "ui-preferences");
}

#[test]
fn scroll_speed_is_quarter_step_validated_and_preserved_by_other_shortcuts() {
    let (settings, writer, mut context) = service();
    let store = SettingsBackedUiPreferences::new(settings);
    let initial = store.load().unwrap();
    assert_eq!(initial.preferences.scroll_speed(), 1.0);
    assert!(initial.preferences.clone().with_scroll_speed(1.1).is_err());
    assert!(
        initial
            .preferences
            .clone()
            .with_scroll_speed(10.25)
            .is_err()
    );

    let configured = store.store_scroll_speed(2.25, initial.revision).unwrap();
    assert_eq!(configured.preferences.scroll_speed(), 2.25);
    let themed = store
        .store_theme("heycode-high-contrast", configured.revision)
        .unwrap();
    assert_eq!(themed.preferences.scroll_speed(), 2.25);
    assert_eq!(writer.stored.lock().unwrap().len(), 2);
    context.shutdown();
}

#[test]
fn focus_view_persists_without_resetting_other_ui_preferences() {
    let (settings, writer, mut context) = service();
    let store = SettingsBackedUiPreferences::new(settings);
    let initial = store.load().unwrap();
    let configured = store.store_scroll_speed(2.25, initial.revision).unwrap();

    let focused = store.store_focus_view(true, configured.revision).unwrap();

    assert!(focused.preferences.focus_view());
    assert_eq!(focused.preferences.scroll_speed(), 2.25);
    assert_eq!(focused.preferences.theme_id(), "heycode-dark");
    assert!(!focused.preferences.vim_mode());
    assert_eq!(writer.stored.lock().unwrap().len(), 2);
    context.shutdown();
}

#[test]
fn full_response_copy_preference_is_revision_checked_and_preserved_by_theme_changes() {
    let (settings, writer, mut context) = service();
    let store = SettingsBackedUiPreferences::new(settings);
    let initial = store.load().unwrap();
    assert!(!initial.preferences.copy_full_response());
    let enabled = store
        .store(
            &initial.preferences.clone().with_copy_full_response(true),
            initial.revision,
        )
        .unwrap();
    assert!(enabled.preferences.copy_full_response());
    assert_eq!(
        writer.stored.lock().unwrap().last().unwrap()["copy_full_response"],
        true
    );
    let themed = store
        .store_theme("heycode-high-contrast", enabled.revision)
        .unwrap();
    assert!(themed.preferences.copy_full_response());
    let before = writer.stored.lock().unwrap().len();
    assert!(store.store(&initial.preferences, initial.revision).is_err());
    assert_eq!(writer.stored.lock().unwrap().len(), before);
    let restored = store
        .store(
            &themed.preferences.with_copy_full_response(false),
            themed.revision,
        )
        .unwrap();
    assert!(!restored.preferences.copy_full_response());
    assert_eq!(restored.preferences.theme_id(), "heycode-high-contrast");
    context.shutdown();
}

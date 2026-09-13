//! U14 settings browser and CAS mutation boundary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use heycode_core::Context;
use heycode_settings::{
    SettingsDefinition, SettingsDocuments, SettingsFieldPath, SettingsNamespace, SettingsSchema,
    SettingsService, SettingsWriter,
};
use heycode_tui::settings_panel::{SettingsPanelValue, SettingsPanelView};
use heycode_ui::UiContributionId;
use heycode_ui::settings_ui::SettingsUiRegistry;
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingWriter {
    writes: Mutex<Vec<(String, Value)>>,
}

impl SettingsWriter for RecordingWriter {
    fn persist_user(&self, namespace: &SettingsNamespace, section: &Value) -> Result<(), String> {
        self.writes
            .lock()
            .unwrap()
            .push((namespace.as_str().to_owned(), section.clone()));
        Ok(())
    }
}

fn setup() -> (
    Arc<SettingsService>,
    Arc<SettingsUiRegistry>,
    Arc<RecordingWriter>,
    Context,
) {
    let writer = Arc::new(RecordingWriter::default());
    let settings = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        writer.clone(),
    ));
    let context = Context::new();
    let schema = SettingsSchema::new(
        json!({"type":"object","properties":{
            "enabled":{"type":"boolean"},
            "label":{"type":"string"},
            "mode":{"type":"string","enum":["safe","fast"]},
            "retries":{"type":"integer"},
            "api_key":{"type":"string"},
            "freeform":{"type":"object"}
        }}),
        json!({
            "enabled":false,"label":"demo","mode":"safe","retries":2,
            "api_key":null,"freeform":{}
        }),
        |value| {
            let object = value
                .as_object()
                .ok_or_else(|| "must be object".to_owned())?;
            if !object.get("enabled").is_some_and(Value::is_boolean)
                || !object.get("label").is_some_and(Value::is_string)
                || !object.get("mode").is_some_and(Value::is_string)
                || !object.get("retries").is_some_and(Value::is_number)
            {
                return Err("typed fields are invalid".to_owned());
            }
            Ok(())
        },
    )
    .unwrap()
    .with_secret_path(SettingsFieldPath::new("api_key").unwrap());
    settings
        .register(
            &context,
            SettingsDefinition::new(SettingsNamespace::new("demo").unwrap(), schema),
        )
        .unwrap();
    (
        settings,
        Arc::new(SettingsUiRegistry::new()),
        writer,
        context,
    )
}

fn select(view: &mut SettingsPanelView, path: &str) {
    for _ in 0..view.rows().len() {
        if view.rows()[view.selected()].path() == path {
            return;
        }
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
    }
    panic!("missing settings row `{path}`");
}

#[test]
fn browser_renders_every_schema_field_without_secret_values_or_silent_omission() {
    let (settings, ui, _writer, _context) = setup();
    let view = SettingsPanelView::open(settings, ui).unwrap();
    assert_eq!(view.rows().len(), 6);
    let secret = view
        .rows()
        .iter()
        .find(|row| row.path() == "api_key")
        .unwrap();
    assert!(matches!(
        secret.value(),
        SettingsPanelValue::Secret { configured: false }
    ));
    assert!(!secret.editable());
    assert!(!format!("{secret:?}").contains("sk-"));
    let freeform = view
        .rows()
        .iter()
        .find(|row| row.path() == "freeform")
        .unwrap();
    assert!(matches!(
        freeform.value(),
        SettingsPanelValue::Unrenderable { .. }
    ));
    assert!(freeform.explanation().is_some());
}

#[test]
fn toggle_choice_text_and_number_updates_commit_through_revision_cas() {
    let (settings, ui, writer, _context) = setup();
    let mut view = SettingsPanelView::open(settings.clone(), ui).unwrap();

    select(&mut view, "enabled");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    select(&mut view, "mode");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    select(&mut view, "label");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..4 {
        view.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    }
    for character in "saved".chars() {
        view.handle_key(KeyCode::Char(character), KeyModifiers::NONE);
    }
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    select(&mut view, "retries");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    view.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    view.handle_key(KeyCode::Char('7'), KeyModifiers::NONE);
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);

    let snapshot = settings
        .get(&SettingsNamespace::new("demo").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.resolved()["enabled"], true);
    assert_eq!(snapshot.resolved()["mode"], "fast");
    assert_eq!(snapshot.resolved()["label"], "saved");
    assert_eq!(snapshot.resolved()["retries"], 7);
    assert_eq!(snapshot.revision(), 4);
    assert_eq!(writer.writes.lock().unwrap().len(), 4);
    assert_eq!(view.notice(), Some("saved and applied"));
}

#[test]
fn stale_browser_write_reports_conflict_and_custom_surface_stays_visible() {
    let (settings, ui, writer, context) = setup();
    let mut stale = SettingsPanelView::open(settings.clone(), ui.clone()).unwrap();
    settings
        .replace_user(
            &SettingsNamespace::new("demo").unwrap(),
            json!({"enabled":true,"label":"external","mode":"safe","retries":2}),
            Some(0),
        )
        .unwrap();
    select(&mut stale, "enabled");
    stale.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        stale
            .notice()
            .unwrap()
            .contains("changed since it was read")
    );
    assert_eq!(writer.writes.lock().unwrap().len(), 1);

    ui.register_custom(
        &context,
        &SettingsNamespace::new("demo").unwrap(),
        UiContributionId::new("demo-settings").unwrap(),
    )
    .unwrap();
    let custom = SettingsPanelView::open(settings, ui).unwrap();
    assert!(matches!(
        custom.rows()[0].value(),
        SettingsPanelValue::Custom { panel } if panel == "demo-settings"
    ));
    assert!(
        custom.rows()[0]
            .explanation()
            .unwrap()
            .contains("custom panel")
    );
}

#[test]
fn shell_chrome_preferences_are_typed_settings_rows_and_persist_live_values() {
    let writer = Arc::new(RecordingWriter::default());
    let settings = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        writer.clone(),
    ));
    let context = Context::new();
    settings
        .register(
            &context,
            heycode_ui::preferences::settings_definition().unwrap(),
        )
        .unwrap();
    let mut view =
        SettingsPanelView::open(settings.clone(), Arc::new(SettingsUiRegistry::new())).unwrap();

    let header = view
        .rows()
        .iter()
        .find(|row| row.path() == "header_density")
        .unwrap();
    assert!(matches!(
        header.value(),
        SettingsPanelValue::Choice { options, selected }
            if options == &["full".to_owned(), "compact".to_owned()]
                && selected.as_deref() == Some("full")
    ));
    for path in ["footer_status", "footer_hints"] {
        let row = view.rows().iter().find(|row| row.path() == path).unwrap();
        assert!(matches!(row.value(), SettingsPanelValue::Toggle(true)));
    }
    let pet = view
        .rows()
        .iter()
        .find(|row| row.path() == "header_pet")
        .unwrap();
    assert!(matches!(pet.value(), SettingsPanelValue::Toggle(true)));

    select(&mut view, "header_density");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    select(&mut view, "footer_status");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    select(&mut view, "header_pet");
    view.handle_key(KeyCode::Enter, KeyModifiers::NONE);

    let namespace = heycode_ui::preferences::settings_namespace().unwrap();
    let committed = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(committed.resolved()["header_density"], "compact");
    assert_eq!(committed.resolved()["footer_status"], false);
    assert_eq!(committed.resolved()["footer_hints"], true);
    assert_eq!(committed.resolved()["header_pet"], false);
    assert_eq!(writer.writes.lock().unwrap().len(), 3);
    assert_eq!(view.notice(), Some("saved and applied"));
}

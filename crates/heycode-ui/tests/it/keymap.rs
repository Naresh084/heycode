//! U20: the chord grammar, the conflict rule, and a keymap that survives a
//! restart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_core::{Context, compose};
use heycode_settings::{
    SettingsDocuments, SettingsNamespace, SettingsService, SettingsWriter, settings_plugin,
};
use heycode_ui::keymap::{
    KeyChord, KeyName, Keymap, KeymapAction, KeymapError, Modifiers, SettingsBackedKeymap,
    keymap_plugin, settings_definition, settings_namespace,
};
use serde_json::{Value, json};

/// Captures whatever the settings stack persists, so a test can rebuild a
/// process from exactly the bytes a real writer would have stored.
#[derive(Default)]
struct RecordingWriter {
    stored: Mutex<Option<Value>>,
}

impl SettingsWriter for RecordingWriter {
    fn persist_user(&self, _namespace: &SettingsNamespace, section: &Value) -> Result<(), String> {
        *self.stored.lock().unwrap() = Some(section.clone());
        Ok(())
    }
}

/// Register the keymap namespace over one user document, returning the
/// settings layer's own refusal message when it declines.
fn register_user_document(user: Value) -> Result<(), String> {
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(settings_namespace().unwrap(), user)
        .map_err(|error| error.to_string())?;
    let service = SettingsService::new(documents);
    let mut context = Context::default();
    let outcome = service.register(&context, settings_definition().unwrap());
    context.shutdown();
    outcome.map(|_| ()).map_err(|error| error.to_string())
}

/// A settings service holding one keymap namespace, optionally pre-loaded with
/// a user document.
fn service(user: Option<Value>) -> (Arc<SettingsService>, Arc<RecordingWriter>, Context) {
    let mut documents = SettingsDocuments::new();
    if let Some(user) = user {
        documents
            .set_user(settings_namespace().unwrap(), user)
            .unwrap();
    }
    let writer = Arc::new(RecordingWriter::default());
    let service = Arc::new(SettingsService::with_writer(
        documents,
        writer.clone() as Arc<dyn SettingsWriter>,
    ));
    let context = Context::default();
    service
        .register(&context, settings_definition().unwrap())
        .unwrap();
    (service, writer, context)
}

#[test]
fn modifier_names_are_case_insensitive() {
    let canonical = KeyChord::parse("ctrl+p").unwrap();
    for spelling in ["Ctrl+p", "CTRL+p", "ctrl+p"] {
        assert_eq!(KeyChord::parse(spelling).unwrap(), canonical, "{spelling}");
    }
    assert_eq!(canonical.to_string(), "ctrl+p");
}

#[test]
fn an_uppercase_key_letter_becomes_lowercase_plus_an_explicit_shift() {
    // Otherwise `ctrl+P` and `ctrl+shift+p` would be two map entries for one
    // physical chord, and a conflict between them would go undetected.
    let upper = KeyChord::parse("ctrl+P").unwrap();
    assert_eq!(upper.to_string(), "ctrl+shift+p");
    assert_eq!(upper, KeyChord::parse("ctrl+shift+p").unwrap());
    assert_ne!(
        upper,
        KeyChord::parse("ctrl+p").unwrap(),
        "shift must stay significant"
    );
}

#[test]
fn modifiers_render_in_canonical_order_whatever_order_they_were_written() {
    let chord = KeyChord::parse("shift+alt+ctrl+f5").unwrap();
    assert_eq!(chord.to_string(), "ctrl+alt+shift+f5");
    assert_eq!(chord.key(), KeyName::Function(5));
    assert_eq!(
        chord,
        KeyChord::parse("ctrl+alt+shift+f5").unwrap(),
        "two orderings must not become two map entries"
    );
}

#[test]
fn plus_and_space_are_named_because_plus_separates_modifiers() {
    assert_eq!(
        KeyChord::parse("ctrl+plus").unwrap().key(),
        KeyName::Char('+')
    );
    assert_eq!(KeyChord::parse("space").unwrap().key(), KeyName::Char(' '));
    assert_eq!(
        KeyChord::new(KeyName::Char('+'), Modifiers::ctrl()).to_string(),
        "ctrl+plus",
        "the rendered form must parse back"
    );
}

#[test]
fn chord_text_outside_the_grammar_is_rejected_by_name() {
    for text in [
        "",
        " ctrl+p",
        "hyper+p",
        "ctrl+ctrl+p",
        "f13",
        "ctrl+",
        "pp",
    ] {
        let error = KeyChord::parse(text).unwrap_err();
        assert!(
            matches!(&error, KeymapError::InvalidChord { chord } if chord == text),
            "`{text}` produced {error}"
        );
    }
    assert!(KeyChord::parse("f12").is_ok(), "f12 is in range");
}

#[test]
fn every_shipped_binding_is_distinct() {
    let map = Keymap::defaults();
    let mut seen = BTreeMap::new();
    for action in KeymapAction::ALL {
        let chord = map.chord(action);
        assert!(
            seen.insert(chord, action).is_none(),
            "{} duplicates {}",
            action.as_str(),
            chord
        );
    }
    assert_eq!(seen.len(), KeymapAction::ALL.len());
    assert_eq!(
        map.chord(KeymapAction::CommandPalette).to_string(),
        "ctrl+p"
    );
    assert_eq!(map.chord(KeymapAction::QueueFollowUp).to_string(), "tab");
}

#[test]
fn two_actions_on_one_chord_are_reported_naming_both() {
    let mut overrides = BTreeMap::new();
    overrides.insert(KeymapAction::Submit, KeyChord::parse("ctrl+q").unwrap());
    overrides.insert(KeymapAction::Quit, KeyChord::parse("ctrl+q").unwrap());
    let error = Keymap::resolve(&overrides).unwrap_err();
    assert_eq!(
        error.to_string(),
        "`ctrl+q` is bound to both `submit` and `quit`",
        "a conflict must name the chord and both actions, not pick a winner"
    );
}

#[test]
fn an_override_colliding_with_an_untouched_default_is_still_a_conflict() {
    // The case a precedence rule hides: nothing rebound the palette, so a
    // naive "overrides win" would silently disable it.
    let mut overrides = BTreeMap::new();
    overrides.insert(KeymapAction::Submit, KeyChord::parse("ctrl+p").unwrap());
    let error = Keymap::resolve(&overrides).unwrap_err();
    assert_eq!(
        error.to_string(),
        "`ctrl+p` is bound to both `submit` and `command-palette`"
    );
}

#[test]
fn the_session_host_detach_chord_cannot_be_bound() {
    let mut overrides = BTreeMap::new();
    overrides.insert(KeymapAction::Submit, KeyChord::parse("ctrl+]").unwrap());
    let error = Keymap::resolve(&overrides).unwrap_err();
    assert_eq!(
        error.to_string(),
        "`ctrl+]` is reserved by the session host for detach"
    );

    let message = register_user_document(json!({"bindings": {"submit": "ctrl+]"}}))
        .expect_err("a host-owned chord must be refused before persistence");
    assert!(message.contains("reserved by the session host for detach"));
}

#[test]
fn an_override_that_frees_its_old_chord_resolves() {
    // Swapping two bindings is legal; only a genuine double-binding is not.
    let mut overrides = BTreeMap::new();
    overrides.insert(KeymapAction::Submit, KeyChord::parse("ctrl+p").unwrap());
    overrides.insert(
        KeymapAction::CommandPalette,
        KeyChord::parse("ctrl+k").unwrap(),
    );
    let map = Keymap::resolve(&overrides).unwrap();
    assert_eq!(
        map.action(KeyChord::parse("ctrl+p").unwrap()),
        Some(KeymapAction::Submit)
    );
    assert_eq!(
        map.action(KeyChord::parse("ctrl+k").unwrap()),
        Some(KeymapAction::CommandPalette)
    );
    assert_eq!(map.action(KeyChord::parse("ctrl+z").unwrap()), None);
}

#[test]
fn only_bindings_that_differ_from_the_defaults_are_persisted() {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        KeymapAction::CommandPalette,
        KeyChord::parse("ctrl+k").unwrap(),
    );
    let map = Keymap::resolve(&overrides).unwrap();
    let persisted = map.overrides();
    assert_eq!(persisted.len(), 1, "today's defaults must not be frozen");
    assert_eq!(
        persisted
            .get(&KeymapAction::CommandPalette)
            .map(ToString::to_string),
        Some("ctrl+k".to_owned())
    );
}

#[test]
fn a_rebound_key_survives_a_restart() {
    // The acceptance criterion: change a binding, persist it through the
    // settings stack, then rebuild the world from exactly the stored bytes.
    let (settings, writer, mut context) = service(None);
    let store = SettingsBackedKeymap::new(settings.clone());
    assert_eq!(store.load().unwrap(), Keymap::defaults());

    let mut overrides = BTreeMap::new();
    overrides.insert(
        KeymapAction::CommandPalette,
        KeyChord::parse("ctrl+k").unwrap(),
    );
    store.store(&Keymap::resolve(&overrides).unwrap()).unwrap();
    let stored = writer.stored.lock().unwrap().clone().expect("a write");
    context.shutdown();
    drop(store);
    drop(settings);

    let (restarted, _writer, mut restarted_context) = service(Some(stored));
    let reloaded = SettingsBackedKeymap::new(restarted).load().unwrap();
    assert_eq!(
        reloaded.chord(KeymapAction::CommandPalette).to_string(),
        "ctrl+k"
    );
    assert_eq!(
        reloaded.chord(KeymapAction::Submit).to_string(),
        "enter",
        "an untouched action keeps its default across the restart"
    );
    restarted_context.shutdown();
}

#[test]
fn a_stale_keymap_editor_cannot_overwrite_a_newer_generation() {
    let (settings, _writer, mut context) = service(None);
    let store = SettingsBackedKeymap::new(settings.clone());
    let (loaded, revision) = store.load_versioned().unwrap();
    settings
        .replace_user(
            &settings_namespace().unwrap(),
            json!({"bindings": {"command-palette": "ctrl+k"}}),
            Some(revision),
        )
        .unwrap();

    let error = store
        .store_at(&loaded, revision)
        .expect_err("stale keymap write must not win");
    assert!(error.to_string().contains("changed since"), "{error}");
    context.shutdown();
}

#[test]
fn a_stored_document_this_build_cannot_read_fails_loud() {
    // Skipping an unknown row would quietly restore the default binding, so
    // the user would lose a key they had deliberately rebound.
    let message = register_user_document(json!({"bindings": {"teleport": "ctrl+t"}}))
        .expect_err("an unknown action must not register");
    assert!(
        message.contains("unknown keymap action `teleport`"),
        "expected the offending id to be named, got {message}"
    );
}

#[test]
fn a_stored_chord_outside_the_grammar_fails_loud() {
    let message = register_user_document(json!({"bindings": {"submit": "hyper+z"}}))
        .expect_err("an invalid chord must not register");
    assert!(
        message.contains("`hyper+z` is not a key chord"),
        "expected the offending text to be named, got {message}"
    );
}

#[test]
fn a_conflicting_document_is_refused_before_it_becomes_the_live_keymap() {
    let message = register_user_document(json!({"bindings": {"submit": "ctrl+p"}}))
        .expect_err("a conflicting user document must not register");
    assert!(
        message.contains("`ctrl+p` is bound to both `submit` and `command-palette`"),
        "expected the conflict to be named, got {message}"
    );
}

#[test]
fn the_keymap_namespace_registers_exactly_once_in_a_real_composition() {
    let plugins: Vec<Box<dyn heycode_core::Plugin>> =
        vec![settings_plugin(SettingsDocuments::new()), keymap_plugin()];
    let mut context = compose(&plugins).unwrap();
    let rows: Vec<&'static str> = context
        .plugin_inventory()
        .snapshot()
        .unwrap()
        .contributions
        .iter()
        .filter(|row| {
            row.kind == heycode_core::ContributionKind::SettingsNamespace && row.name == "keymap"
        })
        .map(|row| row.plugin)
        .collect();
    assert_eq!(rows, vec!["keymap"]);
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    assert!(
        settings
            .get(&settings_namespace().unwrap())
            .unwrap()
            .is_some()
    );
    context.shutdown();
    assert!(
        settings
            .get(&settings_namespace().unwrap())
            .unwrap()
            .is_none(),
        "the namespace registration is an effect and must unwind"
    );
}

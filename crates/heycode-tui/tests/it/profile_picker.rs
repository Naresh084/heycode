//! K07 real picker consumer over the effect-owned shared profile store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use heycode_config::{NamedProfileService, SERVICE_PROFILES, named_profiles_plugin};
use heycode_core::compose;
use heycode_tui::app::{AppState, TuiRunOutcome};

fn profile(name: &str, plugin: &str) -> String {
    format!(
        "schema_version = 1\nname = \"{name}\"\n[[plugins]]\nid = \"{plugin}\"\nenabled = false\n"
    )
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn picker_lists_the_shared_store_highlights_current_and_recomposes_selected() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("profiles");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("beta.toml"), profile("beta", "mcp")).unwrap();
    std::fs::write(root.join("alpha.toml"), profile("alpha", "skills")).unwrap();
    let context = compose(&[named_profiles_plugin(home.path())]).unwrap();
    let profiles = context
        .get::<NamedProfileService>(SERVICE_PROFILES)
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_profiles(profiles, Some("beta".to_owned()));

    state.apply(&heycode_agent::UiEvent::ProfilePickerRequested);
    let picker = state.profile_picker().expect("picker is open");
    assert_eq!(
        picker
            .rows()
            .iter()
            .map(|row| (row.label(), row.current))
            .collect::<Vec<_>>(),
        [
            ("built-in (no profile)", false),
            ("alpha", false),
            ("beta", true)
        ],
        "the built-in composition is always offered, and the active row is marked"
    );
    assert_eq!(picker.selected(), 2, "current profile is highlighted");

    state.handle_terminal_event(&key(KeyCode::Up));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert_eq!(
        state.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeProfile {
            name: Some("alpha".to_owned())
        })
    );

    // Choosing the row that is already active is a no-op close, not a restart.
    state.apply(&heycode_agent::UiEvent::ProfilePickerRequested);
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(state.profile_picker().is_none());
    assert_eq!(state.take_run_outcome(), None);

    // The built-in row recomposes without any profile.
    state.apply(&heycode_agent::UiEvent::ProfilePickerRequested);
    state.handle_terminal_event(&key(KeyCode::Up));
    state.handle_terminal_event(&key(KeyCode::Up));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert_eq!(
        state.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeProfile { name: None })
    );
}

#[test]
fn with_no_named_profiles_the_picker_still_shows_the_built_in_row_as_current() {
    let home = tempfile::tempdir().unwrap();
    let context = compose(&[named_profiles_plugin(home.path())]).unwrap();
    let profiles = context
        .get::<NamedProfileService>(SERVICE_PROFILES)
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_profiles(profiles, None);
    state.apply(&heycode_agent::UiEvent::ProfilePickerRequested);
    let picker = state.profile_picker().expect("picker is open");
    assert_eq!(picker.rows().len(), 1);
    assert!(picker.rows()[0].current);
    assert_eq!(picker.selected(), 0);
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert_eq!(
        state.take_run_outcome(),
        None,
        "already on the built-in composition"
    );
}

#[test]
fn invalid_direct_selection_is_recoverable_and_never_requests_recomposition() {
    let home = tempfile::tempdir().unwrap();
    let context = compose(&[named_profiles_plugin(home.path())]).unwrap();
    let profiles = context
        .get::<NamedProfileService>(SERVICE_PROFILES)
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_profiles(profiles, None);

    state.apply(&heycode_agent::UiEvent::ProfileSelected {
        name: "missing".to_owned(),
    });
    assert!(state.take_run_outcome().is_none());
    assert!(matches!(
        state.items.last(),
        Some(heycode_tui::app::Item::Error(_))
    ));
}

#[test]
fn approval_preempts_and_closes_the_profile_picker() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("profiles");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("alpha.toml"), profile("alpha", "skills")).unwrap();
    let context = compose(&[named_profiles_plugin(home.path())]).unwrap();
    let profiles = context
        .get::<NamedProfileService>(SERVICE_PROFILES)
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_profiles(profiles, None);
    state.apply(&heycode_agent::UiEvent::ProfilePickerRequested);
    assert!(state.profile_picker().is_some());

    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 7,
        name: "bash".to_owned(),
        args_preview: "command requested".to_owned(),
    });

    assert!(state.profile_picker().is_none());
    assert!(state.pending_ask.is_some());
}

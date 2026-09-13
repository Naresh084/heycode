//! Masked authorization input rendering and routing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_api_key::{InteractiveSecretPrompt, SecretPromptNotification};
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};
use heycode_tui::app::AppState;
use ratatui::{Terminal, backend::TestBackend};

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(76, 18)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn secret_prompt_starts_empty_and_shows_only_prefix_and_suffix() {
    let bus = heycode_core::EventBus::default();
    let prompt = Arc::new(InteractiveSecretPrompt::new(bus));
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_secret_prompt(prompt.clone());
    state.apply_secret_prompt(&SecretPromptNotification::Requested {
        error: None,
        id: 7,
        prompt: "Paste OpenRouter API key".to_owned(),
        query: CredentialQuery::new(
            CredentialReference::new("OPENROUTER_API_KEY").unwrap(),
            CredentialKind::new("api-key").unwrap(),
        ),
        operation: None,
        masked: true,
    });
    let empty = frame(&mut state);
    assert!(!empty.contains("••"), "no prefilled secret: {empty}");
    assert!(empty.contains("Type or paste a key"));
    for character in "top-secret".chars() {
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Char(character)));
    }
    let text = frame(&mut state);
    assert!(text.contains("Paste OpenRouter API key"), "{text}");
    assert!(text.contains("top-s••••t"), "{text}");
    assert!(!text.contains("top-secret"), "{text}");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.pending_secret.is_none());
}

#[test]
fn retry_resets_input_and_long_keys_keep_the_suffix_visible() {
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    let request = |id, error| SecretPromptNotification::Requested {
        id,
        prompt: "Paste your OpenRouter API key".into(),
        error,
        query: CredentialQuery::new(
            CredentialReference::new("OPENROUTER_API_KEY").unwrap(),
            CredentialKind::new("api-key").unwrap(),
        ),
        operation: None,
        masked: true,
    };
    state.apply_secret_prompt(&request(1, None));
    for character in "short".chars() {
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Char(character)));
    }
    let text = frame(&mut state);
    assert!(!text.contains("short"));
    assert!(text.contains("•••••"));
    state.apply_secret_prompt(&request(2, None));
    let long = format!("first{}9", "private".repeat(100));
    state.handle_terminal_event(&crossterm::event::Event::Paste(long.clone()));
    let text = frame(&mut state);
    assert!(text.contains("first••"));
    assert!(text.contains("•9"), "suffix remains visible: {text}");
    assert!(!text.contains("private"));
    state.apply_secret_prompt(&request(
        3,
        Some("API key is invalid. Enter another key.".into()),
    ));
    let text = frame(&mut state);
    assert!(text.contains("API key is invalid"));
    assert!(!text.contains('•'), "retry starts empty: {text}");
    assert!(!text.contains("first"));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(state.pending_secret.is_none());
}

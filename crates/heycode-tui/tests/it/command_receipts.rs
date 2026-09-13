//! Command echo rows and the receipts that close them.
//!
//! The shapes pinned here are read from the Claude Code 2.1.269 captures under
//! `tmp/terminal-checks/20260912T023224Z-memory-skills-reference-{dark,light,
//! no-color}` and `tmp/terminal-evidence/workspace-cd-claude-journey-*`: a
//! command echo on a prompt-background band, the settled outcome directly
//! below it as `  ⎿  text` with no blank row between, wrapped rows aligned
//! under the text column, and an unrecognised slash line answered by a
//! standalone warning-coloured `⏺` row with no echo at all.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_tui::app::{AppState, Item};
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

fn state_with(items: Vec<Item>) -> AppState {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items = items;
    state
}

/// Trimmed transcript rows, oldest first, with the surrounding chrome removed.
fn rows(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The rows from the echo of `command` through the end of its block.
fn block(rows: &[String], command: &str) -> Vec<String> {
    let start = rows
        .iter()
        .position(|row| row.starts_with(&format!("❯ {command}")))
        .unwrap_or_else(|| panic!("no echo for {command}: {rows:#?}"));
    rows[start..]
        .iter()
        .take_while(|row| !row.is_empty())
        .cloned()
        .collect()
}

#[test]
fn a_command_receipt_sits_directly_under_its_echo() {
    let mut state = state_with(vec![
        Item::Command("/reload-skills".to_owned()),
        Item::Info("Reloaded skills: 14 skills available (1 added)".to_owned()),
    ]);
    let drawn = rows(&mut state, 126, 20);
    assert_eq!(
        block(&drawn, "/reload-skills"),
        vec![
            "❯ /reload-skills".to_owned(),
            "  ⎿  Reloaded skills: 14 skills available (1 added)".to_owned(),
        ],
        "{drawn:#?}"
    );
}

#[test]
fn a_command_failure_uses_the_same_receipt_as_a_command_success() {
    let mut state = state_with(vec![
        Item::Command("/cd".to_owned()),
        Item::Error("Usage: /cd <path>".to_owned()),
    ]);
    let drawn = rows(&mut state, 126, 20);
    assert_eq!(
        block(&drawn, "/cd"),
        vec!["❯ /cd".to_owned(), "  ⎿  Usage: /cd <path>".to_owned(),],
        "{drawn:#?}"
    );
}

#[test]
fn a_wrapped_receipt_aligns_under_its_text_column() {
    let text = "Couldn't find a directory at ".to_owned() + &"long-path-segment/".repeat(6);
    let mut state = state_with(vec![
        Item::Command("/cd missing".to_owned()),
        Item::Info(text),
    ]);
    let drawn = rows(&mut state, 60, 24);
    let receipt = block(&drawn, "/cd missing");
    assert!(receipt.len() > 2, "the receipt must wrap: {receipt:#?}");
    assert!(receipt[1].starts_with("  ⎿  "), "{receipt:#?}");
    for row in &receipt[2..] {
        assert!(
            row.starts_with("     ") && !row.starts_with("      "),
            "continuation rows align under the text column: {receipt:#?}"
        );
    }
    assert!(
        receipt.iter().all(|row| row.chars().count() <= 60),
        "{receipt:#?}"
    );
}

#[test]
fn an_info_line_without_a_command_above_it_stays_flush_left() {
    let mut state = state_with(vec![
        Item::Assistant("answer".to_owned()),
        Item::Info("standalone note".to_owned()),
    ]);
    let drawn = rows(&mut state, 126, 20);
    assert!(
        drawn.iter().any(|row| row == "standalone note"),
        "{drawn:#?}"
    );
    assert!(!drawn.iter().any(|row| row.contains('⎿')), "{drawn:#?}");
}

#[test]
fn an_unknown_command_is_a_warning_row_rather_than_an_error_row() {
    let mut state = state_with(vec![Item::Notice(
        "Unknown command: /skill-doctor".to_owned(),
    )]);
    let mut terminal = Terminal::new(TestBackend::new(126, 20)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = (0..20)
        .find(|y| {
            (0..126)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains("Unknown command: /skill-doctor")
        })
        .expect("notice row");
    let text = (0..126)
        .map(|x| buffer[(x, row)].symbol())
        .collect::<String>();
    assert_eq!(text.trim_end(), "⏺ Unknown command: /skill-doctor");
    let styles = state.styles();
    assert_eq!(buffer[(0, row)].fg, styles.warn(), "the glyph is a warning");
    assert_eq!(buffer[(2, row)].fg, styles.warn(), "so is its text");
}

#[test]
fn the_receipt_leader_is_dim_and_its_text_is_not() {
    let mut state = state_with(vec![
        Item::Command("/skills".to_owned()),
        Item::Info("No changes".to_owned()),
    ]);
    let mut terminal = Terminal::new(TestBackend::new(126, 20)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = (0..20)
        .find(|y| {
            (0..126)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains("No changes")
        })
        .expect("receipt row");
    let styles = state.styles();
    assert_eq!(buffer[(2, row)].symbol(), "⎿");
    assert_eq!(buffer[(2, row)].fg, styles.dim());
    assert_eq!(buffer[(5, row)].fg, styles.text());
    assert_eq!(
        buffer[(5, row)].bg,
        buffer[(120, row)].bg,
        "a receipt carries no band of its own"
    );
}

#[test]
fn the_command_band_is_the_width_of_its_own_text() {
    let mut state = state_with(vec![Item::Command("/skills".to_owned())]);
    let mut terminal = Terminal::new(TestBackend::new(126, 20)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = (0..20)
        .find(|y| {
            (0..126)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .starts_with("❯ /skills")
        })
        .expect("command row");
    let band = state.styles().prompt_background();
    let width = (0..126).filter(|x| buffer[(*x, row)].bg == band).count();
    assert_eq!(width, "❯ /skills".chars().count() + 1, "band {width} cells");
}

/// Dismissing a routing picker that a slash line opened closes that echo.
///
/// `Kept model as …` and `Cancelled` are the Claude Code 2.1.269 wordings for
/// `/model` and `/effort` followed by Escape, captured in
/// `tmp/terminal-checks/20260912T015543Z-claude-routing-light`.
#[test]
fn dismissing_a_routing_picker_closes_the_echo_that_opened_it() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    let mut state = state_with(vec![Item::Command("/effort".to_owned())]);
    state.apply(&heycode_agent::UiEvent::EffortPickerRequested {
        owner: heycode_agent::BackendControlOwner::NativeInference {
            provider: "native".to_owned(),
        },
        routing_revision: 9,
        current_effort: Some("medium".to_owned()),
        choices: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        default_effort: Some("medium".to_owned()),
    });
    state.handle_terminal_event(&escape);
    assert!(matches!(
        state.items.last(),
        Some(Item::Info(text)) if text == "Cancelled"
    ));
    let drawn = rows(&mut state, 126, 20);
    assert_eq!(
        block(&drawn, "/effort"),
        vec!["❯ /effort".to_owned(), "  ⎿  Cancelled".to_owned()],
        "{drawn:#?}"
    );

    // A picker nobody asked for by name has no echo to close.
    let mut keyboard = state_with(vec![Item::Assistant("answer".to_owned())]);
    keyboard.apply(&heycode_agent::UiEvent::EffortPickerRequested {
        owner: heycode_agent::BackendControlOwner::NativeInference {
            provider: "native".to_owned(),
        },
        routing_revision: 9,
        current_effort: Some("medium".to_owned()),
        choices: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        default_effort: Some("medium".to_owned()),
    });
    keyboard.handle_terminal_event(&escape);
    assert!(
        !keyboard
            .items
            .iter()
            .any(|item| matches!(item, Item::Info(text) if text == "Cancelled")),
        "{:?}",
        keyboard.items
    );
}

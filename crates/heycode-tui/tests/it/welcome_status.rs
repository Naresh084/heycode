//! Startup identity stays visual while diagnostics remain available to flat/status surfaces.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_tui::app::{AppState, WelcomeHealth, WelcomeStatusView};
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
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
fn startup_uses_the_identity_header_without_a_diagnostic_welcome_table() {
    let mut state = AppState::new("ignored", "/ignored".into());
    state.set_welcome(WelcomeStatusView::new(
        "native",
        "openrouter",
        "stealth/ox-alpha",
        "ask",
        "/workspace/repo".into(),
    ));
    let frame = frame_text(&mut state);
    for expected in [
        "heycode",
        "stealth/ox-alpha",
        "heycode native",
        "/workspace/repo",
    ] {
        assert!(frame.contains(expected), "missing {expected:?}: {frame}");
    }
    for removed in [
        "Welcome to heycode",
        "Runtime",
        "Route",
        "Health",
        "openrouter",
    ] {
        assert!(
            !frame.contains(removed),
            "startup telemetry leaked as content: {frame}"
        );
    }
    assert!(frame.contains("⏸ manual mode on"), "{frame}");

    state.apply(&heycode_agent::UiEvent::UserEcho {
        text: "hello".to_owned(),
    });
    let conversation = frame_text(&mut state);
    assert!(conversation.contains("❯ hello"), "{conversation}");
}

#[test]
fn health_details_stay_in_accessible_diagnostics_instead_of_idle_content() {
    let mut state = AppState::new("m", "/workspace".into());
    state.set_welcome(WelcomeStatusView::new(
        "native",
        "deepseek",
        "deepseek-v4-flash",
        "deny",
        "/workspace".into(),
    ));
    state.set_welcome_health(WelcomeHealth::Unhealthy {
        failed: 2,
        skipped: 1,
    });
    let frame = frame_text(&mut state);
    assert!(!frame.contains("unhealthy"), "{frame}");
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(
        flat.contains("health: unhealthy; 2 failed; 1 skipped"),
        "{flat}"
    );

    state.set_welcome_health(WelcomeHealth::Unavailable);
    assert!(!frame_text(&mut state).contains("unavailable"));
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("health: unavailable"), "{flat}");
}

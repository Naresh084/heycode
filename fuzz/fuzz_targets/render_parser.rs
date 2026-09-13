#![no_main]

use std::path::PathBuf;

use heycode_agent::UiEvent;
use heycode_tui::{AppState, ScreenReaderSnapshot};
use libfuzzer_sys::fuzz_target;
use ratatui::{Terminal, backend::TestBackend};

const MAX_INPUT_BYTES: usize = 32 * 1024;
const MAX_FLAT_LINES: usize = 512;
const MAX_FLAT_LINE_CHARS: usize = 2_048;

fn state_with_markdown(markdown: &str) -> AppState {
    let mut state = AppState::new("fuzz-model", PathBuf::from("/workspace"));
    state.apply(&UiEvent::TurnStarted { turn: 1 });
    state.apply(&UiEvent::AssistantDelta {
        text: markdown.to_owned(),
    });
    state.apply(&UiEvent::TurnFinished {
        reason: "stop".to_owned(),
        usage: None,
        context_tokens: None,
    });
    state
}

fn terminal_symbols(terminal: &Terminal<TestBackend>) -> Vec<String> {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect()
}

fn render_symbols(markdown: &str, width: u16, height: u16) -> Option<(Vec<String>, Vec<String>)> {
    let backend = TestBackend::new(width, height);
    let Ok(mut terminal) = Terminal::new(backend) else {
        return None;
    };
    let mut state = state_with_markdown(markdown);
    if terminal
        .draw(|frame| heycode_tui::render::draw(frame, &mut state))
        .is_err()
    {
        return None;
    }
    let first = terminal_symbols(&terminal);
    if terminal
        .draw(|frame| heycode_tui::render::draw(frame, &mut state))
        .is_err()
    {
        return None;
    }
    Some((first, terminal_symbols(&terminal)))
}

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let width = 20_u16.saturating_add(u16::from(input.first().copied().unwrap_or(0) % 81));
    let height = 8_u16.saturating_add(u16::from(input.get(1).copied().unwrap_or(0) % 33));
    let markdown = String::from_utf8_lossy(input);

    let Some((first, second)) = render_symbols(&markdown, width, height) else {
        panic!("bounded in-memory TUI rendering must succeed");
    };
    assert!(first == second, "TUI Markdown rendering must be idempotent");
    assert!(
        first.len() == usize::from(width).saturating_mul(usize::from(height)),
        "TUI rendering must stay inside the fixed terminal buffer"
    );
    assert!(
        first
            .iter()
            .flat_map(|symbol| symbol.chars())
            .all(|character| !character.is_control()),
        "rendered terminal cells must not contain control characters"
    );

    let state = state_with_markdown(&markdown);
    let snapshot = ScreenReaderSnapshot::from_state(&state);
    let lines = snapshot.as_text().lines().collect::<Vec<_>>();
    assert!(
        lines.len() <= MAX_FLAT_LINES,
        "screen-reader projection must respect its line cap"
    );
    assert!(
        lines
            .iter()
            .all(|line| line.chars().count() <= MAX_FLAT_LINE_CHARS),
        "screen-reader projection must respect its per-line cap"
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| line.chars())
            .all(|character| !character.is_control()),
        "screen-reader projection must sanitize control characters"
    );
});

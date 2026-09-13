//! Deterministic TUI journey recorder shared by integration tests.

#![allow(dead_code, clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_tui::app::AppState;
use heycode_tui::app::accessibility::ScreenReaderSnapshot;
use heycode_tui::terminal::TuiDisplayMode;
use heycode_ui::terminal::TerminalEnvironment;

/// One explicit action/event step and the stable frame it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JourneyFrame {
    /// One-based step number.
    pub step: usize,
    /// `action`, `terminal`, or `ui`.
    pub kind: &'static str,
    /// Human contract label for the exact stimulus.
    pub label: String,
    /// Screen-reader frame after the stimulus settled synchronously.
    pub frame: String,
}

/// Replays explicit actions and events against the real [`AppState`].
///
/// It owns no clock, terminal task, provider transport, or global environment
/// hook. Every captured frame comes from the production screen-reader
/// projection of the same state the interactive loop renders.
pub struct JourneyHarness {
    state: AppState,
    frames: Vec<JourneyFrame>,
    replacements: Vec<(String, String)>,
}

impl JourneyHarness {
    /// Start a journey in explicit screen-reader mode.
    #[must_use]
    pub fn new(mut state: AppState) -> Self {
        let environment = TerminalEnvironment::new()
            .with_term(Some("xterm-256color"))
            .with_colorterm(Some("truecolor"))
            .with_columns(Some(100));
        let capabilities = TuiDisplayMode::ScreenReader.resolve(&environment);
        state.apply_terminal(capabilities, &heycode_ui::theme::default_theme().unwrap());
        Self {
            state,
            frames: Vec::new(),
            replacements: Vec::new(),
        }
    }

    /// Replace one nondeterministic typed fact in every later frame.
    pub fn normalize(&mut self, actual: impl Into<String>, stable: impl Into<String>) {
        self.replacements.push((actual.into(), stable.into()));
    }

    /// Apply one explicit synchronous host action and capture its frame.
    pub fn action(&mut self, label: impl Into<String>, action: impl FnOnce(&mut AppState)) {
        action(&mut self.state);
        self.capture("action", label.into());
    }

    /// Deliver one real terminal event through the production key router.
    pub fn terminal(&mut self, label: impl Into<String>, event: crossterm::event::Event) {
        let _exit = self.state.handle_terminal_event(&event);
        self.capture("terminal", label.into());
    }

    /// Deliver one live UI event through the production state reducer.
    pub fn ui(&mut self, label: impl Into<String>, event: heycode_agent::UiEvent) {
        self.state.apply(&event);
        self.capture("ui", label.into());
    }

    /// Current mutable application state for typed fixture setup/assertions.
    #[must_use]
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// Captured steps in replay order.
    #[must_use]
    pub fn frames(&self) -> &[JourneyFrame] {
        &self.frames
    }

    /// Stable transcript containing explicit steps and their full frames.
    #[must_use]
    pub fn transcript(&self) -> String {
        self.frames
            .iter()
            .map(|frame| {
                format!(
                    "== step {:02} {}: {} ==\n{}",
                    frame.step, frame.kind, frame.label, frame.frame
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn capture(&mut self, kind: &'static str, label: String) {
        let mut frame = ScreenReaderSnapshot::from_state(&self.state).into_text();
        for (actual, stable) in &self.replacements {
            frame = frame.replace(actual, stable);
        }
        self.frames.push(JourneyFrame {
            step: self.frames.len() + 1,
            kind,
            label,
            frame,
        });
    }
}

/// One ordinary key press.
#[must_use]
pub fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    key_with(code, crossterm::event::KeyModifiers::NONE)
}

/// One key press with explicit modifiers.
#[must_use]
pub fn key_with(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

//! U09 effective permission/sandbox picker state, keyboard and frame contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_exec::{
    FileReadScope, FileWriteScope, NetworkScope, SandboxCapabilityReport, SandboxChoiceCapability,
    SandboxMode,
};
use heycode_tui::app::AppState;
use heycode_tui::permission_picker::build_permission_rows;
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

fn report() -> SandboxCapabilityReport {
    SandboxCapabilityReport {
        effective_mode: SandboxMode::Off,
        active_backend: None,
        available_backend: Some("seatbelt"),
        choices: vec![
            SandboxChoiceCapability {
                mode: SandboxMode::Off,
                selectable: true,
                file_read: FileReadScope::Host,
                file_write: FileWriteScope::Host,
                network: NetworkScope::Host,
            },
            SandboxChoiceCapability {
                mode: SandboxMode::ReadOnly,
                selectable: false,
                file_read: FileReadScope::Unspecified,
                file_write: FileWriteScope::Unspecified,
                network: NetworkScope::Unspecified,
            },
            SandboxChoiceCapability {
                mode: SandboxMode::WorkspaceWrite,
                selectable: true,
                file_read: FileReadScope::Host,
                file_write: FileWriteScope::WorkspaceAndTemp,
                network: NetworkScope::Host,
            },
        ],
    }
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
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

use heycode_agent::{
    Command, CommandDescriptor, CommandRegistry, CommandSource, CommandTiming, UiEvent,
};
struct MenuCommand(CommandDescriptor);

#[async_trait::async_trait]
impl Command for MenuCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.0
    }
    async fn execute(&self, _: &heycode_agent::Agent, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn state() -> AppState {
    let mut state = AppState::new("model-a", std::env::temp_dir().join("heycode-ui-proof"));
    state.provider = "provider-a".to_owned();
    state.runtime = "native".to_owned();
    state.permission = "ask".to_owned();
    state.apply(&UiEvent::Info {
        text: "Existing conversation remains visible".to_owned(),
    });
    let mut commands = CommandRegistry::new();
    for (id, description) in [
        ("model", "Choose a model"),
        ("settings", "Configure heycode"),
        ("permissions", "Change approval mode"),
    ] {
        commands
            .register(std::sync::Arc::new(MenuCommand(
                CommandDescriptor::new(
                    id,
                    description,
                    Vec::new(),
                    CommandTiming::Immediate,
                    CommandSource::from_plugin("commands").unwrap(),
                )
                .unwrap(),
            )))
            .unwrap();
    }
    state.set_commands(std::sync::Arc::new(commands));
    state
}

#[test]
fn four_modes_have_short_descriptions_and_auto_is_absent() {
    let rows = build_permission_rows("ask", true);
    assert_eq!(
        rows.iter().map(|r| r.label).collect::<Vec<_>>(),
        vec!["Full access", "Accepted edits", "Default", "Plan"]
    );
    assert!(rows[2].current);
    assert!(rows.iter().all(|row| row.selectable));
    assert!(rows.iter().all(|r| !r.description.contains("tool")));
}

#[test]
fn frame_is_plain_language_and_highlights_the_committed_mode() {
    let mut state = state();
    state.open_permission_picker(report());
    let text = frame_text(&mut state);
    for expected in [
        "Permissions",
        "Full access",
        "Accepted edits",
        "Default",
        "We'll ask for permission each time.",
        "Default ✔",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    for absent in [
        "Auto",
        "sandbox",
        "backend",
        "filesystem",
        "Read Only",
        "Workspace Write",
    ] {
        assert!(!text.contains(absent), "{text}");
    }
}

#[test]
fn selection_dispatches_the_real_command_without_claiming_early_success() {
    let mut state = state();
    state.input.insert_str("draft");
    state.open_permission_picker(report());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.pending_send.take().as_deref(),
        Some("/permissions accepted_edits")
    );
    assert_eq!(state.permission, "ask");
    assert_eq!(state.input.lines(), &["draft"]);
    assert!(state.permission_picker().is_none());
    state.open_permission_picker(report());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.permission_picker().is_none());
    assert_eq!(state.pending_send.as_deref(), Some("/permissions plan"));
    assert_eq!(state.permission, "ask");
}

#[test]
fn approval_preempts_permissions_and_escape_preserves_state() {
    let mut state = state();
    state.open_permission_picker(report());
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 7,
        name: "read".into(),
        args_preview: "a.txt".into(),
    });
    assert!(state.permission_picker().is_none());
    state.pending_ask = None;
    state.open_permission_picker(report());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(state.permission_picker().is_none());
    assert!(state.pending_send.is_none());
}

#[test]
fn fixed_connection_cannot_offer_a_selectable_mode() {
    assert!(
        build_permission_rows("ask", false)
            .iter()
            .all(|row| !row.selectable)
    );
}

#[test]
fn narrow_menu_keeps_the_selected_choice_and_navigation_visible() {
    let mut state = state();
    state.open_permission_picker(report());
    for _ in 0..4 {
        let selected = state.permission_picker().unwrap().selected();
        let label = state.permission_picker().unwrap().rows()[selected].label;
        let mut terminal = Terminal::new(TestBackend::new(64, 22)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains(&format!("❯ {}. {label}", selected + 1)),
            "{text}"
        );
        assert!(
            text.contains("↑/↓ to navigate · Enter to select · Esc to cancel"),
            "{text}"
        );
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    }
}

#[test]
fn plan_picker_allows_an_explicit_user_mode_change() {
    let rows = build_permission_rows("plan", true);
    assert!(rows[3].current && rows[3].selectable);
    assert!(
        rows[..3]
            .iter()
            .all(|row| row.selectable && row.unavailable_reason.is_none())
    );
}

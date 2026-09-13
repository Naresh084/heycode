//! U04 fuzzy catalog projection, keyboard interaction and frame contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandRegistry,
    CommandSource, CommandTiming,
};
use heycode_tui::app::AppState;
use heycode_tui::command_palette::filter_commands;
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

struct PaletteCommand {
    descriptor: CommandDescriptor,
    availability: CommandAvailability,
}

#[async_trait]
impl Command for PaletteCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        self.availability.clone()
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, _args: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn registry() -> Arc<CommandRegistry> {
    let mut registry = CommandRegistry::new();
    let commands = CommandSource::from_plugin("commands").unwrap();
    let deploy = CommandSource::from_plugin("deploy-plugin").unwrap();
    for command in [
        PaletteCommand {
            descriptor: CommandDescriptor::new(
                "model",
                "Switch the active model",
                vec![CommandArgument::optional("id", "Model id").unwrap()],
                CommandTiming::Queued,
                commands.clone(),
            )
            .unwrap(),
            availability: CommandAvailability::available(),
        },
        PaletteCommand {
            descriptor: CommandDescriptor::new(
                "status",
                "Show effective runtime, route and permission",
                Vec::new(),
                CommandTiming::Immediate,
                commands.clone(),
            )
            .unwrap(),
            availability: CommandAvailability::available(),
        },
        PaletteCommand {
            descriptor: CommandDescriptor::new(
                "exit",
                "Exit heycode",
                Vec::new(),
                CommandTiming::Immediate,
                commands.clone(),
            )
            .unwrap(),
            availability: CommandAvailability::available(),
        },
        PaletteCommand {
            descriptor: CommandDescriptor::new(
                "plugins",
                "Inspect installed extensions",
                vec![CommandArgument::optional("view", "Optional view").unwrap()],
                CommandTiming::Immediate,
                commands,
            )
            .unwrap(),
            availability: CommandAvailability::available(),
        },
        PaletteCommand {
            descriptor: CommandDescriptor::new(
                "deploy",
                "Ship to an environment",
                Vec::new(),
                CommandTiming::Interrupting,
                deploy,
            )
            .unwrap()
            .with_shortcut("Ctrl+D")
            .unwrap(),
            availability: CommandAvailability::unavailable("Connect deployment runtime").unwrap(),
        },
    ] {
        registry.register(Arc::new(command)).unwrap();
    }
    Arc::new(registry)
}

fn key(
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

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
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
fn fuzzy_ranking_covers_exact_prefix_subsequence_typo_description_and_source() {
    let catalog = registry().catalog().unwrap();
    for (query, expected) in [
        ("plugins", "plugins"),
        ("plu", "plugins"),
        ("pgs", "plugins"),
        ("plguins", "plugins"),
        ("extensions", "plugins"),
        ("deploy-plugin", "deploy"),
    ] {
        let matches = filter_commands(&catalog, query);
        assert_eq!(matches[0].entry.descriptor.id(), expected, "{query}");
    }
    assert!(
        filter_commands(&catalog, "").iter().any(|row| {
            row.entry.descriptor.id() == "deploy" && !row.entry.availability.is_available()
        }),
        "unavailable commands stay discoverable"
    );
}

#[test]
fn slash_and_ctrl_p_open_filter_render_select_and_close_the_palette() {
    let mut state = AppState::new("m", "/workspace".into());
    state.set_commands(registry());
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Char('/'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(state.command_palette().is_some());
    let initial = frame_text(&mut state);
    assert!(initial.contains("/model [id]"), "{initial}");
    assert!(initial.contains("/deploy"), "{initial}");
    assert!(initial.contains("Connect deployment runtime"), "{initial}");
    assert!(!initial.contains("deploy-plugin"), "{initial}");
    assert!(!initial.contains("Ctrl+D"), "{initial}");
    assert!(
        initial.find("Connect deployment runtime").unwrap() < initial.rfind("❯ /").unwrap(),
        "command suggestions appear above the composer: {initial}"
    );
    assert!(
        initial.contains("❯ /"),
        "composer uses the reference prompt gutter: {initial}"
    );

    for character in ['p', 'l', 'u'] {
        state.handle_terminal_event(&key(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    let palette = state.command_palette().unwrap();
    assert_eq!(palette.query(), "plu");
    assert_eq!(palette.matches()[0].entry.descriptor.id(), "plugins");
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(state.command_palette().is_none());
    assert_eq!(state.input.lines(), ["/plugins "]);

    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Char('p'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert!(state.command_palette().is_some());
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(state.command_palette().is_none());
    assert_eq!(state.input.lines(), ["/plugins "]);
}

#[test]
fn unavailable_selection_reports_the_reason_and_clears_the_composer() {
    let mut state = AppState::new("m", "/workspace".into());
    state.set_commands(registry());
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Char('p'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    for character in "deploy".chars() {
        state.handle_terminal_event(&key(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    assert!(
        !state.command_palette().unwrap().matches()[0]
            .entry
            .availability
            .is_available()
    );
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    // The reason is reported and the composer is cleared, so the next thing
    // the user types is not appended to a dead command.
    assert!(state.command_palette().is_none());
    assert_eq!(state.input.lines(), [""]);
    assert!(state.items.iter().any(|item| matches!(
        item,
        heycode_tui::app::Item::Error(text) if text.contains("Connect deployment runtime")
    )));
    assert!(state.pending_send.is_none());
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 1,
        name: "bash".to_owned(),
        args_preview: "dangerous".to_owned(),
    });
    assert!(state.command_palette().is_none());
    assert!(state.pending_ask.is_some());
}

fn type_str(state: &mut AppState, text: &str) {
    for character in text.chars() {
        state.handle_terminal_event(&key(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
}

fn enter(state: &mut AppState) {
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
}

fn fresh() -> AppState {
    let mut state = AppState::new("m", "/workspace".into());
    state.set_commands(registry());
    state
}

/// The composer owns the text; the palette is a view over its first token.
/// Claude Code and Codex both run a slash command on the FIRST Enter.
#[test]
fn typing_a_slash_command_keeps_the_text_in_the_composer() {
    let mut state = fresh();
    type_str(&mut state, "/status");
    assert_eq!(state.input.lines(), ["/status"]);
    let palette = state
        .command_palette()
        .expect("palette open while typing a command");
    assert_eq!(palette.query(), "status");
    assert_eq!(
        palette
            .highlighted()
            .map(|index| palette.matches()[index].entry.descriptor.id()),
        Some("status")
    );
}

#[test]
fn one_enter_runs_a_bare_command() {
    let mut state = fresh();
    type_str(&mut state, "/status");
    enter(&mut state);
    assert_eq!(state.pending_send.as_deref(), Some("/status"));
    assert!(state.command_palette().is_none());
    assert_eq!(state.input.lines(), [""]);
}

#[test]
fn one_enter_runs_a_command_with_arguments() {
    let mut state = fresh();
    type_str(&mut state, "/model deepseek-v4-pro");
    enter(&mut state);
    assert_eq!(
        state.pending_send.as_deref(),
        Some("/model deepseek-v4-pro")
    );
    assert!(state.command_palette().is_none());
}

/// `/memory` must never run `/compact` because a description happens to match.
#[test]
fn an_unknown_command_is_reported_and_never_fuzzy_run() {
    let mut state = fresh();
    type_str(&mut state, "/extensions");
    let palette = state.command_palette().unwrap();
    assert!(
        palette.highlighted().is_none(),
        "a description-only match is listed but never preselected"
    );
    enter(&mut state);
    assert_eq!(state.pending_send, None);
    assert!(state.command_palette().is_none());
    assert!(
        state.items.iter().any(|item| matches!(
            item,
            heycode_tui::app::Item::Notice(text) if text == "Unknown command: /extensions"
        )),
        "{:?}",
        state.items
    );
}

#[test]
fn typing_exit_or_a_nearby_unknown_command_never_executes_or_quits_before_enter() {
    for text in ["/exit", "/exist"] {
        let mut state = fresh();
        for character in text.chars() {
            state.handle_terminal_event(&key(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            ));
            let _ = frame_text(&mut state);
            assert!(!state.quit_requested, "typing {text:?} requested exit");
            assert!(
                state.pending_send.is_none(),
                "typing {text:?} executed a command"
            );
        }
        assert_eq!(state.input.lines(), [text]);
        enter(&mut state);
        if text == "/exit" {
            assert_eq!(state.pending_send.as_deref(), Some("/exit"));
        } else {
            assert!(state.pending_send.is_none(), "a fuzzy typo executed /exit");
            assert!(state.items.iter().any(|item| matches!(
                item,
                heycode_tui::app::Item::Notice(message)
                    if message == "Unknown command: /exist"
            )));
        }
    }
}

#[test]
fn pasted_exit_text_stays_a_draft_in_default_and_vim_insert_modes() {
    for vim in [false, true] {
        let mut state = fresh();
        state.set_vim_mode(vim);
        state.handle_terminal_event(&crossterm::event::Event::Paste("/exit".to_owned()));
        assert_eq!(state.input.lines(), ["/exit"]);
        assert!(!state.quit_requested);
        assert!(state.pending_send.is_none());
    }
}

#[test]
fn a_prefix_of_a_bare_command_completes_and_runs_on_one_enter() {
    let mut state = fresh();
    type_str(&mut state, "/sta");
    enter(&mut state);
    assert_eq!(state.pending_send.as_deref(), Some("/status"));
}

#[test]
fn a_prefix_of_an_argument_taking_command_completes_into_the_composer() {
    let mut state = fresh();
    type_str(&mut state, "/mod");
    enter(&mut state);
    assert_eq!(state.input.lines(), ["/model "]);
    assert_eq!(state.pending_send, None);
    assert!(state.command_palette().is_none());
}

#[test]
fn escape_closes_the_palette_and_keeps_the_typed_text() {
    let mut state = fresh();
    type_str(&mut state, "/sta");
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(state.command_palette().is_none());
    assert_eq!(state.input.lines(), ["/sta"]);
}

/// Ctrl+P browses the catalogue over the composer: it saves a non-slash draft,
/// and Esc restores it.
#[test]
fn ctrl_p_saves_and_restores_a_non_slash_draft() {
    let mut state = fresh();
    type_str(&mut state, "hello there");
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Char('p'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert!(state.command_palette().is_some());
    assert_eq!(state.input.lines(), ["/"]);
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(state.command_palette().is_none());
    assert_eq!(state.input.lines(), ["hello there"]);
}

/// Arrowing to a description-only row is an explicit choice and may run it.
#[test]
fn an_arrowed_fuzzy_row_runs_on_enter() {
    let mut state = fresh();
    type_str(&mut state, "/extensions");
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Down,
        crossterm::event::KeyModifiers::NONE,
    ));
    let palette = state.command_palette().unwrap();
    assert_eq!(
        palette
            .highlighted()
            .map(|index| palette.matches()[index].entry.descriptor.id()),
        Some("plugins")
    );
    enter(&mut state);
    // plugins takes an optional argument, so Enter completes it.
    assert_eq!(state.input.lines(), ["/plugins "]);
}

#[test]
fn aliases_rank_and_preselect_the_canonical_row_without_activating_typos() {
    let mut registry = CommandRegistry::new();
    for (name, availability) in [
        ("new", CommandAvailability::available()),
        (
            "connect",
            CommandAvailability::unavailable("Connection unavailable").unwrap(),
        ),
    ] {
        registry
            .register(Arc::new(PaletteCommand {
                descriptor: CommandDescriptor::new(
                    name,
                    "Fixture",
                    Vec::new(),
                    CommandTiming::Immediate,
                    CommandSource::from_plugin("fixture").unwrap(),
                )
                .unwrap(),
                availability,
            }))
            .unwrap();
    }
    let catalog = registry.catalog().unwrap();
    for query in ["clear", "cle", "reset"] {
        let matches = filter_commands(&catalog, query);
        assert_eq!(matches[0].entry.descriptor.id(), "new");
        assert!(matches[0].preselectable);
    }
    let matches = filter_commands(&catalog, "claer");
    assert!(!matches[0].preselectable);
    let matches = filter_commands(&catalog, "login");
    assert_eq!(matches[0].entry.descriptor.id(), "connect");
    assert!(!matches[0].entry.availability.is_available());
    assert_eq!(
        matches[0].entry.availability.reason(),
        Some("Connection unavailable")
    );
}

#[test]
fn control_u_clears_the_composer_prefix_instead_of_undoing_one_keystroke() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut state = AppState::new("m", "/workspace".into());
    state.set_commands(registry());
    for character in "/plugins".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert!(state.command_palette().is_some());
    state.handle_terminal_event(&key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(state.input.lines(), [""]);
    assert!(state.command_palette().is_none());
    state.input = tui_textarea::TextArea::new(vec!["écho tail".to_owned()]);
    state
        .input
        .move_cursor(tui_textarea::CursorMove::Jump(0, 4));
    state.handle_terminal_event(&key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(state.input.lines(), [" tail"]);
}

#[test]
fn add_directory_palette_submission_preserves_the_original_draft_and_cursor() {
    for query in ["add-dir", "add-d"] {
        let mut registry = CommandRegistry::new();
        registry
            .register(Arc::new(PaletteCommand {
                descriptor: CommandDescriptor::new(
                    "add-dir",
                    "Grant directory access",
                    vec![CommandArgument::optional("path", "Directory").unwrap()],
                    CommandTiming::Immediate,
                    CommandSource::from_plugin("workspace").unwrap(),
                )
                .unwrap(),
                availability: CommandAvailability::available(),
            }))
            .unwrap();
        let mut state = fresh();
        state.set_commands(Arc::new(registry));
        type_str(&mut state, "keep my draft");
        state.handle_terminal_event(&key(
            crossterm::event::KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        let cursor = state.input.cursor();
        state.handle_terminal_event(&key(
            crossterm::event::KeyCode::Char('p'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        type_str(&mut state, query);
        enter(&mut state);
        if query == "add-d" {
            assert_eq!(state.input.lines(), ["/add-dir "]);
            enter(&mut state);
        }
        assert_eq!(state.input.lines(), ["keep my draft"]);
        assert_eq!(state.input.cursor(), cursor);
        assert!(state.command_palette().is_none());
    }
}

#[test]
fn help_and_memory_palette_submission_preserve_draft_for_exact_and_prefix_choices() {
    for id in ["help", "memory"] {
        for query in [id, &id[..id.len() - 1]] {
            let mut registry = CommandRegistry::new();
            registry
                .register(Arc::new(PaletteCommand {
                    descriptor: CommandDescriptor::new(
                        id,
                        "Open a read-only panel",
                        Vec::new(),
                        CommandTiming::Immediate,
                        CommandSource::from_plugin("fixture").unwrap(),
                    )
                    .unwrap(),
                    availability: CommandAvailability::available(),
                }))
                .unwrap();
            let mut state = fresh();
            state.set_commands(Arc::new(registry));
            type_str(&mut state, "retain this draft");
            let cursor = state.input.cursor();
            state.handle_terminal_event(&key(
                crossterm::event::KeyCode::Char('p'),
                crossterm::event::KeyModifiers::CONTROL,
            ));
            type_str(&mut state, query);
            enter(&mut state);
            assert_eq!(state.input.lines(), ["retain this draft"]);
            assert_eq!(state.input.cursor(), cursor);
            assert!(state.command_palette().is_none());
        }
    }
}

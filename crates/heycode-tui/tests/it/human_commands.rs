//! CMD09/CMD10 human-plane navigation and persisted UI behavior.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_core::Context;
use heycode_settings::{SettingsDocuments, SettingsNamespace, SettingsService, SettingsWriter};
use heycode_tui::app::{AppState, Item, ToolViewState};
use heycode_tui::human_commands::{HumanCommandBridge, HumanCommandRequest, PromptColor};
use heycode_tui::{ScreenReaderSnapshot, render};
use heycode_ui::keymap::Keymap;
use heycode_ui::settings_ui::SettingsUiRegistry;
use heycode_ui::theme::builtin_themes;
use ratatui::{Terminal, backend::TestBackend};

use super::panel_commands::agent_world;

#[derive(Default)]
struct RecordingWriter {
    writes: Mutex<Vec<(String, serde_json::Value)>>,
}

impl SettingsWriter for RecordingWriter {
    fn persist_user(
        &self,
        namespace: &SettingsNamespace,
        section: &serde_json::Value,
    ) -> Result<(), String> {
        self.writes
            .lock()
            .unwrap()
            .push((namespace.as_str().to_owned(), section.clone()));
        Ok(())
    }
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        code,
        crossterm::event::KeyModifiers::NONE,
    ))
}

fn state_with_bridge() -> (AppState, HumanCommandBridge) {
    let bridge = HumanCommandBridge::new();
    let mut state = AppState::new("model", "/workspace".into());
    state.set_human_commands(bridge.clone());
    (state, bridge)
}

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| render::draw(frame, state)).unwrap();
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
fn diff_copy_and_mention_mutate_only_the_human_ui_state() {
    let (mut state, bridge) = state_with_bridge();
    state
        .items
        .push(Item::Assistant("older completed answer".to_owned()));
    state
        .items
        .push(Item::Assistant("completed answer".to_owned()));
    state.apply_session_event(&heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 1,
        time_ms: 1,
        kind: heycode_session::SessionEventKind::AssistantChunk {
            turn: 1,
            step: 1,
            text: Some("still streaming".to_owned()),
            reasoning: None,
        },
    });

    // With no recorded edit diff the panel would open empty, so `/diff`
    // settles with a receipt instead; a recorded diff opens the panel.
    bridge.request(HumanCommandRequest::OpenDiff);
    state.poll_human_command();
    assert_eq!(state.side_panel_kind(), None);
    assert!(
        heycode_tui::ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("no edit or write has produced a diff yet")
    );
    state.apply(&heycode_agent::UiEvent::ToolStarted {
        name: "edit".to_owned(),
        args: serde_json::json!({"path": "a.rs"}),
    });
    state.apply(&heycode_agent::UiEvent::ToolFinished {
        name: "edit".to_owned(),
        ok: true,
        value: serde_json::json!({"diff": "-old\n+new"}),
        untrusted_content: None,
    });
    bridge.request(HumanCommandRequest::OpenDiff);
    state.poll_human_command();
    assert_eq!(
        state.side_panel_kind(),
        Some(heycode_tui::side_panel::SidePanelKind::Diff)
    );

    bridge.request(HumanCommandRequest::CopyAnswer { latest_index: 0 });
    state.poll_human_command();
    assert_eq!(
        state.take_clipboard_request().as_deref(),
        Some("completed answer")
    );

    bridge.request(HumanCommandRequest::CopyAnswer { latest_index: 1 });
    state.poll_human_command();
    assert_eq!(
        state.take_clipboard_request().as_deref(),
        Some("older completed answer")
    );

    bridge.request(HumanCommandRequest::Mention(Some("src/lib.rs".to_owned())));
    state.poll_human_command();
    assert_eq!(state.input.lines().join("\n"), "@src/lib.rs");
}

#[tokio::test]
async fn copy_command_parses_one_based_answer_selection_without_creating_history() {
    let world = agent_world();
    let settings = Arc::new(SettingsService::new(SettingsDocuments::new()));
    let bridge = HumanCommandBridge::new();
    bridge.attach();
    let commands = heycode_tui::human_commands::commands(
        heycode_agent::CommandSource::from_plugin("tui").unwrap(),
        bridge.clone(),
        settings,
        Arc::new(heycode_ui::UiRegistry::new()),
        None,
    )
    .unwrap();
    let copy = commands
        .iter()
        .find(|command| command.descriptor().id() == "copy")
        .unwrap();
    assert_eq!(copy.descriptor().synopsis(), "/copy [number]");
    let before = world.agent.session().lock().unwrap().events().len();
    for (args, expected) in [("", 0), ("1", 0), ("2", 1), ("17", 16)] {
        copy.execute(&world.agent, args).await.unwrap();
        assert_eq!(
            bridge.take(),
            Some(HumanCommandRequest::CopyAnswer {
                latest_index: expected,
            })
        );
    }
    for invalid in ["0", "two", "1 2", "-1"] {
        copy.execute(&world.agent, invalid).await.unwrap();
        assert!(
            matches!(bridge.take(), Some(HumanCommandRequest::CopyArgumentError { message }) if message.contains(invalid))
        );
    }
    assert_eq!(world.agent.session().lock().unwrap().events().len(), before);
}

#[tokio::test]
async fn focus_and_color_are_tui_only_typed_session_controls() {
    let world = agent_world();
    let settings = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        Arc::new(RecordingWriter::default()),
    ));
    let mut context = Context::default();
    settings
        .register(
            &context,
            heycode_ui::preferences::settings_definition().unwrap(),
        )
        .unwrap();
    let (mut state, bridge) = state_with_bridge();
    let commands = heycode_tui::human_commands::commands(
        heycode_agent::CommandSource::from_plugin("tui").unwrap(),
        bridge,
        settings,
        Arc::new(heycode_ui::UiRegistry::new()),
        None,
    )
    .unwrap();
    let focus = commands
        .iter()
        .find(|command| command.descriptor().id() == "focus")
        .unwrap();
    let color = commands
        .iter()
        .find(|command| command.descriptor().id() == "color")
        .unwrap();
    assert_eq!(focus.descriptor().synopsis(), "/focus");
    assert_eq!(color.descriptor().synopsis(), "/color [color]");

    let before = world.agent.session().lock().unwrap().events().len();
    focus.execute(&world.agent, "").await.unwrap();
    state.poll_human_command();
    assert!(state.focus_view());
    assert!(focus.execute(&world.agent, "unexpected").await.is_err());

    color.execute(&world.agent, "cyan").await.unwrap();
    state.poll_human_command();
    assert_eq!(state.prompt_color(), Some(PromptColor::Cyan));
    color.execute(&world.agent, "default").await.unwrap();
    state.poll_human_command();
    assert_eq!(state.prompt_color(), None);
    assert!(color.execute(&world.agent, "cyan blue").await.is_err());
    assert_eq!(world.agent.session().lock().unwrap().events().len(), before);
    context.shutdown();
}

#[test]
fn focus_projection_condenses_tools_and_composer_uses_durable_title() {
    let (mut state, bridge) = state_with_bridge();
    state.items.push(Item::Info("old diagnostic".to_owned()));
    state
        .items
        .push(Item::User("Update the reference safely".to_owned()));
    state.items.push(Item::Tool {
        call_id: Some(heycode_core::CallId::from_raw("edit-1")),
        name: "edit".to_owned(),
        args: serde_json::json!({"path": "reference.txt"}),
        result: Some((
            true,
            serde_json::json!({
                "diff_preview": {"inserted_lines": 1, "removed_lines": 1}
            }),
        )),
        untrusted_content: None,
        view: ToolViewState::default(),
    });
    state.items.push(Item::Tool {
        call_id: Some(heycode_core::CallId::from_raw("read-1")),
        name: "read".to_owned(),
        args: serde_json::json!({"path": "reference.txt"}),
        result: Some((true, serde_json::json!({"content": "REFERENCE_NEW"}))),
        untrusted_content: None,
        view: ToolViewState::default(),
    });
    state
        .items
        .push(Item::Assistant("FOCUS_REFERENCE_COMPLETE".to_owned()));
    state.apply_session_event(&heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 1,
        time_ms: 1,
        kind: heycode_session::SessionEventKind::SessionTitle {
            title: "Durable session title".to_owned(),
        },
    });

    bridge.request(HumanCommandRequest::ApplyFocus(true));
    state.poll_human_command();
    bridge.request(HumanCommandRequest::ApplyPromptColor(Some(
        PromptColor::Cyan,
    )));
    state.poll_human_command();
    let rendered = frame_text(&mut state);
    assert!(
        rendered.contains("Update the reference safely"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Edited 1 file +1 -1, read 1 file"),
        "{rendered}"
    );
    assert!(rendered.contains("FOCUS_REFERENCE_COMPLETE"), "{rendered}");
    assert!(rendered.contains("Focus view"), "{rendered}");
    assert!(rendered.contains("Durable session title"), "{rendered}");
    assert!(!rendered.contains("old diagnostic"), "{rendered}");
    let snapshot = ScreenReaderSnapshot::from_state(&state);
    let accessible = snapshot.as_text();
    assert!(
        accessible.contains("== Focus transcript =="),
        "{accessible}"
    );
    assert!(
        accessible.contains("edit succeeded, read succeeded"),
        "{accessible}"
    );
    assert!(
        accessible.contains("session title: Durable session title"),
        "{accessible}"
    );
    assert!(accessible.contains("session color: cyan"), "{accessible}");
    assert!(!accessible.contains("old diagnostic"), "{accessible}");
}

#[tokio::test]
async fn scroll_speed_picker_persists_previews_and_applies_fractional_wheel_motion() {
    use crossterm::event::{KeyCode, MouseEvent, MouseEventKind};

    let world = agent_world();
    let writer = Arc::new(RecordingWriter::default());
    let settings = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        writer.clone() as Arc<dyn SettingsWriter>,
    ));
    let mut context = Context::default();
    settings
        .register(
            &context,
            heycode_ui::preferences::settings_definition().unwrap(),
        )
        .unwrap();
    let (mut state, bridge) = state_with_bridge();
    state.set_settings_services(settings.clone(), Arc::new(SettingsUiRegistry::new()));
    let commands = heycode_tui::human_commands::commands(
        heycode_agent::CommandSource::from_plugin("tui").unwrap(),
        bridge,
        settings,
        Arc::new(heycode_ui::UiRegistry::new()),
        None,
    )
    .unwrap();
    let scroll = commands
        .iter()
        .find(|command| command.descriptor().id() == "scroll-speed")
        .unwrap();
    assert_eq!(scroll.descriptor().synopsis(), "/scroll-speed");
    scroll.execute(&world.agent, "").await.unwrap();
    state.poll_human_command();
    assert_eq!(state.scroll_speed_picker().unwrap().speed(), 1.0);
    let scroll_frame = frame_text(&mut state);
    assert!(
        scroll_frame.contains("■·········  1× per wheel notch (default)"),
        "{scroll_frame}"
    );
    assert!(
        scroll_frame.contains(
            "Scroll to feel it · ←/→ adjust · r reset to default · Enter save · Esc cancel"
        ),
        "{scroll_frame}"
    );
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("== Scroll speed ==")
    );

    state.handle_terminal_event(&crossterm::event::Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 10,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }));
    assert_eq!(state.scroll_speed_picker().unwrap().preview_offset(), 3);
    state.handle_terminal_event(&key(KeyCode::Left));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(state.scroll_speed_picker().is_none());
    assert_eq!(state.scroll_speed(), 0.75);
    assert_eq!(writer.writes.lock().unwrap()[0].1["scroll_speed"], 0.75);

    state.set_scroll_speed(0.25);
    let _ = frame_text(&mut state);
    let wheel = |kind| {
        crossterm::event::Event::Mouse(MouseEvent {
            kind,
            column: 10,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        })
    };
    state.handle_terminal_event(&wheel(MouseEventKind::ScrollUp));
    assert_eq!(state.scroll_from_bottom, 0);
    state.handle_terminal_event(&wheel(MouseEventKind::ScrollUp));
    assert_eq!(state.scroll_from_bottom, 1);
    state.handle_terminal_event(&wheel(MouseEventKind::ScrollDown));
    state.handle_terminal_event(&wheel(MouseEventKind::ScrollDown));
    assert_eq!(state.scroll_from_bottom, 0);
    context.shutdown();
}

#[tokio::test]
async fn statusline_command_persists_and_applies_existing_footer_controls() {
    let world = agent_world();
    let writer = Arc::new(RecordingWriter::default());
    let settings = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        writer.clone() as Arc<dyn SettingsWriter>,
    ));
    let mut context = Context::default();
    settings
        .register(
            &context,
            heycode_ui::preferences::settings_definition().unwrap(),
        )
        .unwrap();
    let (mut state, bridge) = state_with_bridge();
    let commands = heycode_tui::human_commands::commands(
        heycode_agent::CommandSource::from_plugin("tui").unwrap(),
        bridge,
        settings,
        Arc::new(heycode_ui::UiRegistry::new()),
        None,
    )
    .unwrap();
    let statusline = commands
        .iter()
        .find(|command| command.descriptor().id() == "statusline")
        .unwrap();
    assert_eq!(
        statusline.descriptor().synopsis(),
        "/statusline [action] [value]"
    );
    statusline.execute(&world.agent, "off").await.unwrap();
    state.poll_human_command();
    assert!(!state.shell_preferences().footer_status());
    assert!(state.shell_preferences().footer_hints());
    assert_eq!(writer.writes.lock().unwrap()[0].1["footer_status"], false);
    assert!(
        statusline
            .execute(&world.agent, "hints maybe")
            .await
            .is_err()
    );
    context.shutdown();
}

#[test]
fn theme_preview_is_keyboard_complete_and_persists_at_the_open_revision() {
    let writer = Arc::new(RecordingWriter::default());
    let settings = Arc::new(SettingsService::with_writer(
        SettingsDocuments::new(),
        writer.clone() as Arc<dyn SettingsWriter>,
    ));
    let mut context = Context::default();
    settings
        .register(
            &context,
            heycode_ui::preferences::settings_definition().unwrap(),
        )
        .unwrap();
    let (mut state, bridge) = state_with_bridge();
    state.set_settings_services(settings, Arc::new(SettingsUiRegistry::new()));
    state.apply_terminal(
        heycode_ui::terminal::TerminalCapabilities::detect(
            &heycode_ui::terminal::TerminalEnvironment::new()
                .with_term(Some("xterm-256color"))
                .with_columns(Some(100)),
        ),
        &heycode_ui::theme::default_theme().unwrap(),
    );
    bridge.request(HumanCommandRequest::OpenTheme {
        themes: builtin_themes().unwrap(),
        selected_id: "heycode-dark".to_owned(),
        revision: 0,
    });
    state.poll_human_command();
    assert_eq!(state.theme_picker().unwrap().selected(), 0);
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("== Theme ==")
    );
    let theme_frame = frame_text(&mut state);
    assert!(theme_frame.contains("Choose the text style that looks best with your terminal"));
    assert!(
        theme_frame.contains("❯ 1. heycode dark"),
        "the committed theme carries the cursor: {theme_frame}"
    );
    assert!(
        theme_frame.contains("Enter to select · Esc to cancel"),
        "{theme_frame}"
    );
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 9,
        name: "bash".to_owned(),
        args_preview: "cargo test".to_owned(),
    });
    assert!(state.theme_picker().is_none());
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("== Permission requested ==")
    );
    state.apply(&heycode_agent::UiEvent::ApprovalResolved {
        id: 9,
        allowed: false,
    });
    bridge.request(HumanCommandRequest::OpenTheme {
        themes: builtin_themes().unwrap(),
        selected_id: "heycode-dark".to_owned(),
        revision: 0,
    });
    state.poll_human_command();
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    assert_eq!(state.theme_picker().unwrap().selected(), 1);
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.theme_picker().is_none());
    let writes = writer.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "ui-preferences");
    assert_eq!(writes[0].1["theme"], "heycode-light");
    drop(writes);
    context.shutdown();
}

#[test]
fn keymap_browser_hands_off_an_edit_and_vim_normal_mode_does_not_insert_commands() {
    let (mut state, bridge) = state_with_bridge();
    bridge.request(HumanCommandRequest::OpenKeymap {
        keymap: Keymap::defaults(),
        revision: 7,
    });
    state.poll_human_command();
    assert_eq!(state.keymap_picker().unwrap().revision(), 7);
    let keymap_frame = frame_text(&mut state);
    assert!(keymap_frame.contains("Keybindings"), "{keymap_frame}");
    assert!(
        keymap_frame.contains("settings revision 7"),
        "{keymap_frame}"
    );
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("== Keymap ==")
    );
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.keymap_picker().is_none());
    assert!(
        state
            .input
            .lines()
            .join("\n")
            .starts_with("/keymap submit ")
    );

    bridge.request(HumanCommandRequest::ApplyVim(true));
    state.poll_human_command();
    state.input = tui_textarea::TextArea::default();
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(state.vim_enabled());
    assert!(!state.vim_insert());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('h')));
    assert_eq!(state.input.lines().join("\n"), "");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('i')));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('x')));
    assert_eq!(state.input.lines().join("\n"), "x");
}

#[tokio::test]
async fn human_controls_do_not_create_history_and_missing_inspection_runtime_refuses() {
    let world = agent_world();
    let settings = Arc::new(SettingsService::new(SettingsDocuments::new()));
    let mut settings_context = Context::default();
    settings
        .register(
            &settings_context,
            heycode_ui::keymap::settings_definition().unwrap(),
        )
        .unwrap();
    settings
        .register(
            &settings_context,
            heycode_ui::preferences::settings_definition().unwrap(),
        )
        .unwrap();
    let bridge = HumanCommandBridge::new();
    bridge.attach();
    let commands = heycode_tui::human_commands::commands(
        heycode_agent::CommandSource::from_plugin("tui").unwrap(),
        bridge,
        settings,
        Arc::new(heycode_ui::UiRegistry::new()),
        None,
    )
    .unwrap();
    let before = world.agent.session().lock().unwrap().events().len();
    for id in ["diff", "copy", "mention", "theme", "keymap", "vim"] {
        let command = commands
            .iter()
            .find(|command| command.descriptor().id() == id)
            .unwrap();
        let args = match id {
            "mention" => "src/lib.rs",
            "vim" => "toggle",
            _ => "",
        };
        // Read-only settings in this fixture make persistence commands fail;
        // neither success nor refusal may manufacture model/session history.
        let _ = command.execute(&world.agent, args).await;
    }
    assert_eq!(world.agent.session().lock().unwrap().events().len(), before);

    for id in ["review", "ask-advisor", "security-review"] {
        let command = commands
            .iter()
            .find(|command| command.descriptor().id() == id)
            .unwrap();
        assert_eq!(
            command.descriptor().timing(),
            heycode_agent::CommandTiming::ModelScheduling
        );
        assert!(!command.availability().is_available());
        assert!(
            command
                .availability()
                .reason()
                .unwrap()
                .contains("not configured")
        );
        assert!(
            command
                .execute(&world.agent, "focus on correctness")
                .await
                .is_err()
        );
    }
    assert_eq!(world.agent.session().lock().unwrap().events().len(), before);
    settings_context.shutdown();
}

fn chord(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(code, modifiers))
}

fn vim_normal_state() -> AppState {
    let (mut state, bridge) = state_with_bridge();
    state.set_commands(Arc::new(heycode_agent::CommandRegistry::new()));
    bridge.request(HumanCommandRequest::ApplyVim(true));
    state.poll_human_command();
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(state.vim_enabled());
    assert!(!state.vim_insert());
    state
}

#[test]
fn vim_normal_mode_still_routes_every_bound_chord_and_scroll_key() {
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut palette = vim_normal_state();
    palette.handle_terminal_event(&chord(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert!(
        palette.command_palette().is_some(),
        "ctrl+p must open the palette in vim normal mode"
    );

    let mut reasoning = vim_normal_state();
    let before = reasoning.show_reasoning;
    reasoning.handle_terminal_event(&chord(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_ne!(
        reasoning.show_reasoning, before,
        "ctrl+r must toggle reasoning in vim normal mode"
    );

    let mut side = vim_normal_state();
    side.handle_terminal_event(&chord(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(
        side.side_panel_kind().is_some(),
        "ctrl+b must cycle the side panel in vim normal mode"
    );

    for (code, modifiers) in [
        (KeyCode::PageUp, KeyModifiers::NONE),
        (KeyCode::Char('u'), KeyModifiers::CONTROL),
    ] {
        let mut scroll = vim_normal_state();
        scroll.handle_terminal_event(&chord(code, modifiers));
        assert!(
            scroll.scroll_from_bottom > 0,
            "{code:?} must scroll in vim normal mode"
        );
        scroll.handle_terminal_event(&chord(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(
            scroll.scroll_from_bottom,
            usize::MAX,
            "Home must jump to the oldest line in vim normal mode"
        );
        scroll.handle_terminal_event(&chord(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(
            scroll.scroll_from_bottom, 0,
            "End must jump back to newest in vim normal mode"
        );
    }

    let mut quit = vim_normal_state();
    assert!(!quit.handle_terminal_event(&chord(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(
        quit.handle_terminal_event(&chord(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        "ctrl+c twice must quit in vim normal mode"
    );
    assert_eq!(
        quit.input.lines().join("\n"),
        "",
        "a chord must never type into the composer"
    );
}

#[test]
fn vim_normal_mode_stays_normal_while_a_turn_runs() {
    let mut state = vim_normal_state();
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    assert!(state.vim_enabled());
    assert!(!state.vim_insert());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('h')));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('j')));
    assert_eq!(
        state.input.lines().join("\n"),
        "",
        "the mode line says VIM NORMAL, so hjkl must not type during a turn"
    );
    assert!(frame_text(&mut state).contains("VIM NORMAL"));
}

struct InspectionProvider {
    inner: heycode_llm::testing::FakeProvider,
    name: &'static str,
    requests: Arc<Mutex<Vec<(String, heycode_llm::ChatRequest)>>>,
}
impl heycode_llm::Provider for InspectionProvider {
    fn info(&self) -> heycode_llm::ProviderInfo {
        let mut info = self.inner.info();
        info.name = self.name.to_owned();
        info
    }
    fn stream(&self, request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
        self.requests
            .lock()
            .unwrap()
            .push((self.name.to_owned(), request.clone()));
        if self.name == "blocking" {
            Box::pin(futures::stream::pending())
        } else {
            self.inner.stream(request)
        }
    }
}

#[tokio::test]
async fn inspection_commands_run_native_children_deny_mutation_and_honor_explicit_routes() {
    use heycode_llm::{FinishReason, StreamChunk};
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().canonicalize().unwrap();
    let marker = cwd.join("must-not-write");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let scripts = || {
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("write-attempt".to_owned()),
                    name: Some("write".to_owned()),
                    arguments_delta: serde_json::json!({"path":marker,"content":"forbidden"})
                        .to_string(),
                },
                StreamChunk::Finish(FinishReason::ToolCalls),
            ],
            vec![
                StreamChunk::TextDelta("Verified inspection result".to_owned()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
        ]
    };
    let provider = |name, scripts| {
        Arc::new(InspectionProvider {
            inner: heycode_llm::testing::FakeProvider::new(scripts),
            name,
            requests: calls.clone(),
        }) as Arc<dyn heycode_llm::Provider>
    };
    let mut context = heycode_core::compose(&[
        heycode_session::session_plugin(cwd.join("sessions")),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(10),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..Default::default()
        }),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            heycode_llm::LlmSelection {
                provider_name: "initial".to_owned(),
                model: "initial-model".to_owned(),
            },
            vec![
                provider("initial", vec![]),
                provider("blocking", vec![]),
                provider("live", scripts().into_iter().chain(scripts()).collect()),
                provider("explicit", scripts()),
            ],
        ),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::AutoApprove)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::subagent_plugin(cwd.join("sessions"), 3),
        heycode_agent::agent_options_plugin(heycode_agent::AgentOptions {
            cwd: Some(cwd.clone()),
            ..Default::default()
        }),
        heycode_agent::agent_plugin(),
        heycode_agent::subagent_jobs_plugin(),
    ])
    .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    agent.set_provider("live");
    agent.set_model("live-model");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    let commands = heycode_tui::human_commands::commands(
        heycode_agent::CommandSource::from_plugin("tui").unwrap(),
        HumanCommandBridge::new(),
        Arc::new(SettingsService::new(SettingsDocuments::new())),
        Arc::new(heycode_ui::UiRegistry::new()),
        Some(registry.clone()),
    )
    .unwrap();
    // An explicit preset can choose a stronger registered inference route,
    // but its FullAccess/tools/background requests cannot widen this command.
    let builtin = heycode_agent::builtin_native_presets()
        .unwrap()
        .into_iter()
        .find(|preset| preset.id().as_str() == "advisor")
        .unwrap();
    let override_registration = registry
        .register_preset_owned(
            builtin
                .with_config(heycode_agent::SubagentConfig {
                    inference_provider: Some("explicit".to_owned()),
                    model: Some("stronger-model".to_owned()),
                    permissions: heycode_agent::ChildPermissions::FullAccess,
                    background: true,
                    ..Default::default()
                })
                .unwrap(),
        )
        .unwrap();
    for id in ["review", "ask-advisor", "security-review"] {
        let command = commands
            .iter()
            .find(|command| command.descriptor().id() == id)
            .unwrap();
        assert_eq!(
            command.descriptor().timing(),
            heycode_agent::CommandTiming::ModelScheduling
        );
        assert!(command.availability().is_available());
        command
            .execute(&agent, "inspect the current change")
            .await
            .unwrap();
        assert!(
            !marker.exists(),
            "{id} must deny real write dispatch under AutoApprove"
        );
        assert!(!agent.token().is_turn_active());
    }
    let tasks = registry.task_snapshots();
    assert_eq!(tasks.len(), 3);
    for task in tasks {
        assert_eq!(task.provider, "native");
        assert_eq!(task.state, heycode_agent::TaskState::Completed);
        assert!(task.session_id.is_some());
        assert_eq!(task.workspace.as_deref(), cwd.to_str());
        assert_eq!(task.output, "Verified inspection result");
    }
    {
        let requests = calls.lock().unwrap();
        assert_eq!(
            requests.len(),
            6,
            "each command runs the child tool attempt and result, with no parent inference"
        );
        for (index, (provider, request)) in requests.iter().enumerate() {
            let role_instruction = [
                "Review the requested code",
                "Investigate the user's technical question",
                "Inspect the requested code for actionable security defects",
            ][index / 2];
            assert!(
                request
                    .messages
                    .iter()
                    .any(|message| message.role == heycode_llm::Role::System
                        && message.content.contains(role_instruction))
            );
            let explicit = (2..4).contains(&index);
            assert_eq!(provider, if explicit { "explicit" } else { "live" });
            assert_eq!(
                request.model,
                if explicit {
                    "stronger-model"
                } else {
                    "live-model"
                }
            );
            assert!(
                !request
                    .tools
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|tool| ["write", "bash", "task"].contains(&tool.name.as_str()))
            );
            if index % 2 == 1 {
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|message| message.role == heycode_llm::Role::Tool
                            && (message.content.contains("unknown tool")
                                || message.content.contains("denied")
                                || message.content.contains("not available"))),
                    "actual tool refusal must reach child: {:?}",
                    request.messages
                );
            }
        }
    }
    {
        let parent = agent.session().lock().unwrap();
        for id in ["review", "ask-advisor", "security-review"] {
            assert!(parent.events().iter().any(|event| matches!(&event.kind, heycode_session::SessionEventKind::UserMessage { text } if text == &format!("/{id} inspect the current change"))));
        }
        let results = parent
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                heycode_session::SessionEventKind::AssistantMessage { content, .. } => {
                    Some(content)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 3);
        for result in results {
            assert!(result.contains("Verified inspection result") && result.contains("[task_id:"));
        }
        assert_eq!(
            parent
                .events()
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    heycode_session::SessionEventKind::TurnEnd {
                        reason: heycode_session::TurnEndReason::Stop,
                        ..
                    }
                ))
                .count(),
            3
        );
    }
    assert_eq!(events.lock().unwrap().iter().filter(|event| matches!(event, heycode_agent::UiEvent::TurnFinished { reason, .. } if reason == "stop")).count(), 3);
    // Positive control: this parent really can mutate the same path.
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    tools
        .get("write")
        .unwrap()
        .run(
            serde_json::json!({"path":marker,"content":"allowed"}),
            &heycode_tools::ToolCtx::default().with_cwd(cwd),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "allowed");
    // The human turn lease owns cancellation all the way into a quiet child.
    agent.set_provider("blocking");
    let review = commands
        .iter()
        .find(|command| command.descriptor().id() == "review")
        .unwrap()
        .clone();
    let running_agent = agent.clone();
    let running = tokio::spawn(async move {
        review
            .execute(&running_agent, "cancel this inspection")
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while calls.lock().unwrap().len() < 7 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    agent.token().cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(!agent.token().is_turn_active());
    assert!(
        agent
            .session()
            .lock()
            .unwrap()
            .events()
            .iter()
            .any(|event| matches!(
                event.kind,
                heycode_session::SessionEventKind::TurnEnd {
                    reason: heycode_session::TurnEndReason::Aborted,
                    ..
                }
            ))
    );
    assert!(events.lock().unwrap().iter().any(|event| matches!(event, heycode_agent::UiEvent::TurnFinished { reason, .. } if reason == "aborted")));
    // File aliases cannot reroute these guarded commands to an external runtime.
    let external = registry
        .register_preset_owned(
            heycode_agent::SubagentPreset::new(
                "reviewer",
                "External override",
                "Inspect",
                Some(heycode_agent::SubagentProviderId::new("external").unwrap()),
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
            )
            .unwrap(),
        )
        .unwrap();
    let before = agent.session().lock().unwrap().events().len();
    assert!(
        commands
            .iter()
            .find(|command| command.descriptor().id() == "review")
            .unwrap()
            .execute(&agent, "")
            .await
            .unwrap_err()
            .to_string()
            .contains("requires a native preset")
    );
    assert_eq!(agent.session().lock().unwrap().events().len(), before);
    drop(external);
    drop(override_registration);
    context.shutdown();
}

#[test]
fn copy_checks_empty_history_before_argument_errors_and_caps_recent_selection() {
    let (mut state, bridge) = state_with_bridge();
    bridge.request(HumanCommandRequest::CopyArgumentError {
        message: "invalid index".to_owned(),
    });
    state.poll_human_command();
    assert!(
        matches!(state.items.last(), Some(Item::Error(message)) if message == "No assistant message to copy")
    );
    for index in 0..21 {
        state.items.push(Item::Assistant(format!("answer {index}")));
    }
    bridge.request(HumanCommandRequest::CopyArgumentError {
        message: "invalid index".to_owned(),
    });
    state.poll_human_command();
    assert!(matches!(state.items.last(), Some(Item::Error(message)) if message == "invalid index"));
    bridge.request(HumanCommandRequest::CopyAnswer { latest_index: 20 });
    state.poll_human_command();
    assert!(
        matches!(state.items.last(), Some(Item::Error(message)) if message == "Only 20 assistant messages available to copy")
    );
    assert!(state.take_clipboard_request().is_none());
    bridge.request(HumanCommandRequest::CopyAnswer { latest_index: 19 });
    state.poll_human_command();
    assert_eq!(state.take_clipboard_request().as_deref(), Some("answer 1"));
}

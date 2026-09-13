//! Refresh-interleaved navigation through the actual terminal event reducer.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use async_trait::async_trait;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use heycode_tui::{
    app::{AppState, accessibility::ScreenReaderSnapshot},
    task_console::*,
};
use ratatui::{Terminal, backend::TestBackend, style::Modifier};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Source {
    rows: Mutex<Vec<TaskRecord>>,
    unavailable: Mutex<bool>,
}
#[async_trait]
impl TaskSource for Source {
    fn snapshot(&self) -> Result<Vec<TaskRecord>, String> {
        if *self.unavailable.lock().unwrap() {
            Err("temporary disconnect".into())
        } else {
            Ok(self.rows.lock().unwrap().clone())
        }
    }
    fn output(&self, _: &TaskKey, _: Option<u64>, _: usize) -> Result<TaskOutputPage, String> {
        Ok(TaskOutputPage {
            events: (1..81)
                .map(|sequence| TaskOutputEvent {
                    sequence,
                    kind: TaskOutputKind::Text(format!("Retained child output {sequence}")),
                })
                .collect(),
            ..TaskOutputPage::default()
        })
    }
    async fn execute(&self, _: TaskAction, _: CancellationToken) -> Result<String, String> {
        Ok("accepted".into())
    }
}
fn key(id: usize) -> TaskKey {
    TaskKey(format!("child:{id}"))
}
fn setup(count: usize) -> (AppState, Arc<Source>) {
    let source = Arc::new(Source::default());
    *source.rows.lock().unwrap() = (0..count)
        .map(|id| TaskRecord {
            key: key(id),
            kind: TaskKind::Child,
            label: format!("Agent {id}"),
            status: TaskStatus::Running,
            parent: Some("root".into()),
            session: None,
            job: None,
            capabilities: TaskCapabilities {
                output: true,
                steer: true,
                ..TaskCapabilities::default()
            },
            telemetry: TaskTelemetry {
                elapsed_ms: Some(45000),
                ..TaskTelemetry::default()
            },
            detail: None,
        })
        .collect();
    let mut state = AppState::new("fixture-model", "/workspace".into());
    state.set_task_source(source.clone());
    (state, source)
}
fn press(state: &mut AppState, code: KeyCode) {
    state.handle_terminal_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}
fn draw(state: &mut AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}
fn click(state: &mut AppState, wanted: TaskHit) {
    draw(state, 110, 42);
    let rect = state
        .task_console()
        .hits
        .iter()
        .find(|(_, hit)| *hit == wanted)
        .unwrap()
        .0;
    state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    }));
}
fn flat(state: &AppState) -> String {
    ScreenReaderSnapshot::from_state(state).into_text()
}

#[test]
fn agent_navigation_refresh_reorder_and_overflow_keep_target_until_enter() {
    for (width, height) in [(60, 24), (110, 42)] {
        let (mut state, source) = setup(8);
        press(&mut state, KeyCode::Down); // main
        for expected in 0..8 {
            state.refresh_tasks();
            press(&mut state, KeyCode::Down);
            assert_eq!(state.task_console().focused, Some(key(expected)));
            let screen = draw(&mut state, width, height);
            assert!(screen.contains(&format!("Agent {expected}")), "{screen}");
            assert!(!state.task_console().active);
        }
        source.rows.lock().unwrap().reverse();
        state.refresh_tasks();
        assert_eq!(state.task_console().focused, Some(key(7)));
        assert!(flat(&state).contains("Agent navigation focused: Agent 7"));
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.task_console().selected, Some(key(7)));
        assert!(state.task_console().active);
    }
}

#[test]
fn agent_navigation_source_failure_retains_target_and_recovers_without_draft_mutation() {
    let (mut state, source) = setup(3);
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Down);
    *source.unavailable.lock().unwrap() = true;
    state.refresh_tasks();
    assert_eq!(state.task_console().focused, Some(key(0)));
    assert_eq!(state.task_console().records.len(), 3);
    assert!(flat(&state).contains("Showing last known agents"));
    press(&mut state, KeyCode::Down);
    assert_eq!(state.task_console().focused, Some(key(1)));
    *source.unavailable.lock().unwrap() = false;
    source.rows.lock().unwrap().reverse();
    state.refresh_tasks();
    assert_eq!(state.task_console().focused, Some(key(1)));
    assert!(state.task_console().inventory_error.is_none());
    assert!(state.input.lines().iter().all(String::is_empty));
}

#[test]
fn agent_navigation_settlement_moves_to_nearest_then_main_without_composer_focus() {
    let (mut state, source) = setup(3);
    for _ in 0..3 {
        press(&mut state, KeyCode::Down);
    } // Agent 1
    source.rows.lock().unwrap()[1].status = TaskStatus::Completed;
    state.refresh_tasks();
    assert_eq!(state.task_console().focused, Some(key(2)));
    for row in source.rows.lock().unwrap().iter_mut() {
        row.status = TaskStatus::Completed;
    }
    for _ in 0..3 {
        state.refresh_tasks();
        press(&mut state, KeyCode::Down);
        assert!(flat(&state).contains("Agent navigation focused: Main"));
        assert!(draw(&mut state, 60, 24).contains("● main"));
        assert!(state.task_console().focused.is_none());
    }
    press(&mut state, KeyCode::Esc);
    assert!(!flat(&state).contains("Agent navigation focused"));
    assert!(!draw(&mut state, 60, 24).contains("● main"));
}

#[test]
fn agent_navigation_keyboard_and_mouse_main_collapse_and_restore_drafts_and_positions() {
    for keyboard in [false, true] {
        let (mut state, _) = setup(2);
        state.input.insert_str("main first\nmain second");
        state.input.move_cursor(tui_textarea::CursorMove::Head);
        let main_cursor = state.input.cursor();
        state.open_task(key(0));
        state.input.insert_str("child saved");
        state.input.move_cursor(tui_textarea::CursorMove::Head);
        let child_cursor = state.input.cursor();
        click(&mut state, TaskHit::Main);
        assert_eq!(state.input.lines(), &["main first", "main second"]);
        state.open_task(key(0));
        assert_eq!(state.input.lines(), &["child saved"]);
        assert_eq!(state.input.cursor(), child_cursor);
        if keyboard {
            state.open_task(key(1)); // empty child draft permits entering strip
            press(&mut state, KeyCode::Down);
            press(&mut state, KeyCode::Up);
            press(&mut state, KeyCode::Up);
            press(&mut state, KeyCode::Enter);
        } else {
            click(&mut state, TaskHit::Main);
        }
        assert_eq!(state.task_console().view, ConsoleView::Collapsed);
        assert!(!state.task_console().active);
        assert_eq!(state.input.lines(), &["main first", "main second"]);
        assert_eq!(state.input.cursor(), main_cursor);
        state.open_task(key(0));
        assert_eq!(state.input.lines(), &["child saved"]);
        assert_eq!(state.input.cursor(), child_cursor);
    }
}

#[test]
fn agent_navigation_history_inspector_returns_to_its_category_and_owner() {
    let (mut state, _) = setup(3);
    state.input.insert_str("main draft");
    state.open_task(key(0));
    state.input.insert_str("child draft");
    state.open_task_category(TaskCategory::Agents);
    click(&mut state, TaskHit::Open(key(1)));
    assert!(state.task_console().preview);
    press(&mut state, KeyCode::Esc);
    assert_eq!(state.task_console().view, ConsoleView::List);
    assert_eq!(state.task_console().category, TaskCategory::Agents);
    assert_eq!(state.task_console().focused, Some(key(1)));
    assert_eq!(state.foreground_task().unwrap().key, Some(&key(0)));
    assert_eq!(state.input.lines(), &["child draft"]);
    click(&mut state, TaskHit::Open(key(2)));
    press(&mut state, KeyCode::Left);
    assert_eq!(state.task_console().view, ConsoleView::List);
    assert_eq!(state.task_console().category, TaskCategory::Agents);
    assert_eq!(state.task_console().focused, Some(key(2)));
    click(&mut state, TaskHit::Main);
    assert_eq!(state.task_console().view, ConsoleView::Collapsed);
    assert_eq!(state.input.lines(), &["main draft"]);
}

#[test]
fn agent_navigation_caret_and_accessible_keys_follow_actual_keyboard_focus() {
    let (mut state, _) = setup(1);
    draw(&mut state, 60, 24);
    assert!(
        state
            .input
            .cursor_style()
            .add_modifier
            .contains(Modifier::REVERSED)
    );
    press(&mut state, KeyCode::Down);
    draw(&mut state, 60, 24);
    assert!(
        !state
            .input
            .cursor_style()
            .add_modifier
            .contains(Modifier::REVERSED)
    );
    let description = flat(&state);
    assert!(description.contains("Composer not focused; draft preserved"));
    assert!(!description.contains("keys: Enter sends"));
    press(&mut state, KeyCode::Esc);
    draw(&mut state, 60, 24);
    assert!(
        state
            .input
            .cursor_style()
            .add_modifier
            .contains(Modifier::REVERSED)
    );
    assert!(!flat(&state).contains("Composer not focused"));
}

#[test]
fn agent_navigation_child_scroll_position_survives_switch_and_main_return() {
    let (mut state, _) = setup(2);
    state.open_task(key(0));
    draw(&mut state, 60, 24);
    press(&mut state, KeyCode::PageUp);
    let scroll = state.task_console().output_scroll;
    assert!(scroll > 0);
    assert!(!state.task_console().follow);
    state.open_task(key(1));
    state.open_task(key(0));
    assert_eq!(state.task_console().output_scroll, scroll);
    assert!(!state.task_console().follow);
    click(&mut state, TaskHit::Main);
    state.open_task(key(0));
    assert_eq!(state.task_console().output_scroll, scroll);
    assert!(!state.task_console().follow);
}

#[test]
fn agent_navigation_history_refresh_keeps_key_then_nearest_surviving_row() {
    let (mut state, source) = setup(3);
    state.open_task_category(TaskCategory::Agents);
    press(&mut state, KeyCode::Down);
    assert_eq!(state.task_console().focused, Some(key(1)));
    source.rows.lock().unwrap().reverse();
    state.refresh_tasks();
    assert_eq!(state.task_console().focused, Some(key(1)));
    source.rows.lock().unwrap().retain(|row| row.key != key(1));
    state.refresh_tasks();
    assert_eq!(state.task_console().focused, Some(key(0)));
    assert_eq!(state.task_console().view, ConsoleView::List);
}

//! Slash-command routing promises retained conversations, not provider catalogs.
use super::AppState;
use crate::task_console::{
    ConsoleView, TaskCapabilities, TaskCategory, TaskKey, TaskKind, TaskRecord, TaskStatus,
    TaskTelemetry,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

fn draw(state: &mut AppState) -> anyhow::Result<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 42))?;
    terminal.draw(|frame| crate::render::draw(frame, state))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect())
}

#[test]
fn agents_opens_retained_failed_startup_history_and_inspects_exact_record() -> anyhow::Result<()> {
    let mut state = AppState::new("native", "/workspace".into());
    state.input.insert_str("retained parent draft");
    state.task_console.records.push(TaskRecord {
        key: TaskKey("child:failed-startup".into()),
        kind: TaskKind::Child,
        label: "Equinox".into(),
        status: TaskStatus::Failed,
        parent: Some("main".into()),
        session: None,
        job: None,
        capabilities: TaskCapabilities {
            output: true,
            ..Default::default()
        },
        telemetry: TaskTelemetry::default(),
        detail: Some("Provider failed before startup".into()),
    });
    assert!(state.route_task_browser_command("agents", ""));
    assert_eq!(state.task_console.view, ConsoleView::List);
    assert_eq!(state.task_console.category, TaskCategory::Agents);
    assert!(state.capability_catalog().is_none());
    let shown = draw(&mut state)?;
    assert!(shown.contains("Equinox"), "{shown}");
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(state.task_console.preview);
    assert_eq!(
        state.task_console.selected,
        Some(TaskKey("child:failed-startup".into()))
    );
    let shown = draw(&mut state)?;
    assert!(shown.contains("Provider failed before startup"), "{shown}");
    assert_eq!(state.input.lines(), &["retained parent draft"]);
    Ok(())
}

#[test]
fn agents_without_history_opens_sensible_empty_state() -> anyhow::Result<()> {
    let mut state = AppState::new("native", "/workspace".into());
    assert!(state.route_task_browser_command("agents", "  "));
    assert_eq!(state.task_console.category, TaskCategory::Agents);
    let shown = draw(&mut state)?;
    assert!(shown.contains("No agents yet."), "{shown}");
    assert!(state.capability_catalog().is_none());
    Ok(())
}

#[test]
fn explicit_provider_catalog_request_bypasses_history_routing() {
    let mut state = AppState::new("native", "/workspace".into());
    assert!(!state.route_task_browser_command("agents", "providers"));
    assert!(!state.route_task_browser_command("agents", "invalid"));
    assert_eq!(state.task_console.view, ConsoleView::Collapsed);
    assert!(state.route_task_browser_command("tasks", ""));
    assert_eq!(state.task_console.category, TaskCategory::All);
}

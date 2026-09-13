//! Explicit retry controls preserve the selected owner and prior evidence.
use super::AppState;
use crate::task_console::{
    TaskAction, TaskCapabilities, TaskHit, TaskKey, TaskKind, TaskRecord, TaskStatus, TaskTelemetry,
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

fn state(retry: bool, status: TaskStatus) -> AppState {
    let mut state = AppState::new("native", "/workspace".into());
    state.input.insert_str("parent draft");
    state.task_console.records.push(TaskRecord {
        key: TaskKey("child:retained-agent".into()),
        kind: TaskKind::Child,
        label: "Atlas".into(),
        status,
        parent: Some("root".into()),
        session: Some("retained-session".into()),
        job: Some("old-job".into()),
        capabilities: TaskCapabilities {
            output: true,
            retry,
            ..Default::default()
        },
        telemetry: TaskTelemetry::default(),
        detail: Some("permission denied while reading configuration".into()),
    });
    state
        .task_console
        .inspect(TaskKey("child:retained-agent".into()));
    state
}

#[test]
fn explicit_retry_keyboard_and_mouse_keep_owner_draft_and_failure() -> anyhow::Result<()> {
    for mouse in [false, true] {
        let mut state = state(true, TaskStatus::Failed);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 42))?;
        terminal.draw(|frame| crate::render::draw(frame, &mut state))?;
        assert!(
            state.task_console.actions.is_empty(),
            "render must never retry automatically"
        );
        if mouse {
            let rect = state
                .task_console
                .hits
                .iter()
                .find_map(|(rect, hit)| (*hit == TaskHit::Retry).then_some(*rect))
                .ok_or_else(|| anyhow::anyhow!("eligible failure must show retry button"))?;
            state.handle_terminal_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
        } else {
            state.handle_terminal_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('r'),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(
            state.task_console.actions.front(),
            Some(&TaskAction::Retry(TaskKey("child:retained-agent".into())))
        );
        assert_eq!(state.input.lines(), &["parent draft"]);
        assert_eq!(
            state.task_console.records[0].detail.as_deref(),
            Some("permission denied while reading configuration")
        );
        assert_eq!(
            state.task_console.records[0].job.as_deref(),
            Some("old-job")
        );
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        assert_eq!(
            state.task_console.actions.len(),
            1,
            "repeated key before dispatch must not queue another run"
        );
    }
    Ok(())
}

#[test]
fn unavailable_and_settled_states_have_no_retry_button_or_queued_retry() -> anyhow::Result<()> {
    for (retry, status) in [
        (false, TaskStatus::Failed),
        (true, TaskStatus::Running),
        (false, TaskStatus::Cancelled),
        (false, TaskStatus::Interrupted),
        (false, TaskStatus::Completed),
    ] {
        let mut state = state(retry, status);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 42))?;
        terminal.draw(|frame| crate::render::draw(frame, &mut state))?;
        assert!(
            !state
                .task_console
                .hits
                .iter()
                .any(|(_, hit)| *hit == TaskHit::Retry)
        );
        if status == TaskStatus::Failed {
            let shown: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(shown.contains("Retry unavailable"), "{shown}");
        }
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        assert!(state.task_console.actions.is_empty());
        assert!(
            state
                .task_console
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("Retry unavailable"))
        );
    }
    Ok(())
}

#[test]
fn alt_retry_uses_selected_failed_agent_and_raw_binding_stays_separate() {
    let mut state = state(true, TaskStatus::Failed);
    state.task_console.preview = false;
    state.task_console.active = true;
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('t'),
        KeyModifiers::ALT,
    )));
    assert_eq!(
        state.task_console.actions.front(),
        Some(&TaskAction::Retry(TaskKey("child:retained-agent".into())))
    );
    assert!(!state.task_console.raw_output);
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::ALT,
    )));
    assert!(state.task_console.raw_output);
    assert_eq!(state.task_console.actions.len(), 1);
}

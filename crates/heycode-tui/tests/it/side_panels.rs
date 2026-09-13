//! U22 plugin-contributed diff/jobs/agents side-panel interaction.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use heycode_agent::{
    JobRegistry, SubagentContinuation, SubagentPreset, SubagentRegistry, SubagentSeed, UiEvent,
};
use heycode_session::InboxDelivery;
use heycode_tui::app::{AppState, accessibility::ScreenReaderSnapshot};
use heycode_tui::side_panel::SidePanelKind;
use tokio_util::sync::CancellationToken;

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

#[tokio::test]
async fn control_b_cycles_three_live_side_panels_and_back_to_closed() {
    let mut state = AppState::new("model", "/workspace".into());
    state.apply(&UiEvent::ToolStarted {
        name: "edit".to_owned(),
        args: serde_json::json!({"path":"src/lib.rs"}),
    });
    state.apply(&UiEvent::ToolFinished {
        name: "edit".to_owned(),
        ok: true,
        value: serde_json::json!({"diff":"-old\n+new","message":"updated"}),
        untrusted_content: None,
    });
    state.apply(&UiEvent::ToolStarted {
        name: "todo_write".to_owned(),
        args: serde_json::json!({"todos":[
            {"content":"repair composer","status":"completed"},
            {"content":"verify live terminal","status":"in_progress"}
        ]}),
    });
    state.apply(&UiEvent::ToolFinished {
        name: "todo_write".to_owned(),
        ok: true,
        value: serde_json::json!([
            {"content":"repair composer","status":"completed"},
            {"content":"verify live terminal","status":"in_progress"}
        ]),
        untrusted_content: None,
    });

    let jobs = Arc::new(JobRegistry::default());
    let cancellation = CancellationToken::new();
    let child = cancellation.clone();
    let handle = tokio::spawn(async move {
        child.cancelled().await;
    });
    let job_id = jobs
        .admit(
            "review background",
            InboxDelivery::FollowUp,
            cancellation,
            handle,
        )
        .unwrap();
    state.set_job_registry(Some(jobs.clone()));

    let subagents = Arc::new(SubagentRegistry::new());
    let registration = subagents
        .register_preset_owned(
            SubagentPreset::new(
                "reviewer",
                "Reviewer",
                "Review carefully.",
                None,
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
            )
            .unwrap(),
        )
        .unwrap();
    state.set_capability_services(None, Some(subagents), None);

    let chord = || key(KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert!(!state.handle_terminal_event(&chord()));
    assert_eq!(state.side_panel_kind(), Some(SidePanelKind::Diff));
    let diff = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(diff.contains("== Diff =="), "{diff}");
    assert!(diff.contains("+new"), "{diff}");

    assert!(!state.handle_terminal_event(&chord()));
    assert_eq!(state.side_panel_kind(), Some(SidePanelKind::Jobs));
    let jobs_frame = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(jobs_frame.contains("== Tasks =="), "{jobs_frame}");
    assert!(jobs_frame.contains("repair composer"), "{jobs_frame}");
    assert!(jobs_frame.contains("verify live terminal"), "{jobs_frame}");
    assert!(jobs_frame.contains(job_id.as_str()), "{jobs_frame}");
    assert!(jobs_frame.contains("running"), "{jobs_frame}");

    assert!(!state.handle_terminal_event(&chord()));
    assert_eq!(state.side_panel_kind(), Some(SidePanelKind::Agents));
    let agents = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(agents.contains("== Agents =="), "{agents}");
    assert!(agents.contains("reviewer"), "{agents}");

    assert!(!state.handle_terminal_event(&chord()));
    assert_eq!(state.side_panel_kind(), None);
    assert!(
        !ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("== Diff ==")
    );

    assert!(jobs.cancel(&job_id));
    drop(registration);
}

#[test]
fn tui_registers_each_side_panel_as_an_exact_ui_contribution() {
    for (id, title) in [("diff", "Diff"), ("jobs", "Tasks"), ("agents", "Agents")] {
        let descriptor = heycode_tui::side_panel::descriptor(id, title).unwrap();
        assert_eq!(descriptor.slot(), heycode_ui::UiSlot::SidePanel);
        assert_eq!(descriptor.id().as_str(), id);
    }
}

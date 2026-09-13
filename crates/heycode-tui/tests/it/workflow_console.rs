//! Workspace navigation and renderer behavior using the same application reducer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use heycode_tui::{
    app::{AppState, accessibility::ScreenReaderSnapshot},
    workflow_console::*,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct Source {
    rows: Mutex<Vec<WorkflowRun>>,
    actions: Mutex<Vec<WorkflowAction>>,
    fail: Mutex<bool>,
}
#[async_trait]
impl WorkflowSource for Source {
    fn snapshot(&self) -> Result<Vec<WorkflowRun>, String> {
        if *self.fail.lock().unwrap() {
            Err("owner unavailable".into())
        } else {
            Ok(self.rows.lock().unwrap().clone())
        }
    }
    async fn execute(
        &self,
        action: WorkflowAction,
        _: CancellationToken,
    ) -> Result<String, String> {
        self.actions.lock().unwrap().push(action);
        Ok("Owner acknowledged".into())
    }
}
fn run(id: &str) -> WorkflowRun {
    WorkflowRun {
        id: id.into(),
        title: "Release readiness".into(),
        description: "Three focused reviews, one clear release report.".into(),
        status: WorkflowStatus::Running,
        phases: ["Set the scope", "Review in parallel", "Deliver findings"]
            .into_iter()
            .enumerate()
            .map(|(i, title)| WorkflowPhase {
                id: format!("phase-{i}"),
                title: title.into(),
                status: if i == 0 {
                    WorkflowStatus::Completed
                } else if i == 1 {
                    WorkflowStatus::Running
                } else {
                    WorkflowStatus::Queued
                },
                nodes: vec![WorkflowNode {
                    id: format!("node-{i}"),
                    label: title.into(),
                    status: if i == 0 {
                        WorkflowStatus::Completed
                    } else {
                        WorkflowStatus::Running
                    },
                    ..Default::default()
                }],
                agents: if i == 1 {
                    [
                        "Research reliability",
                        "Research performance",
                        "Research usability",
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(a, label)| WorkflowAgent {
                        task_id: format!("task-{a}"),
                        node_id: format!("agent-{a}"),
                        label: label.into(),
                        status: WorkflowStatus::Running,
                        assignment: format!("Review assignment {a}"),
                        summary: (0..60).map(|n| format!("Evidence line {n}.\n")).collect(),
                        ..Default::default()
                    })
                    .collect()
                } else {
                    vec![]
                },
                ..Default::default()
            })
            .collect(),
        controls: WorkflowControls {
            pause: true,
            resume: false,
            stop: true,
        },
        ..Default::default()
    }
}
fn state() -> (AppState, Arc<Source>) {
    let source = Arc::new(Source {
        rows: Mutex::new(vec![run("run-a")]),
        ..Default::default()
    });
    let mut state = AppState::new("native", "/workspace".into());
    state.set_workflow_source(source.clone());
    (state, source)
}
fn press(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!state.handle_terminal_event(&Event::Key(KeyEvent::new(code, modifiers))));
}
fn draw(state: &mut AppState, w: u16, h: u16) -> Vec<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(w))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
}
fn click(state: &mut AppState, rows: &[String], text: &str) {
    let (y, row) = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.contains(text))
        .unwrap();
    let x = row[..row.find(text).unwrap()].chars().count();
    assert!(!state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: u16::try_from(x).unwrap(),
        row: u16::try_from(y).unwrap(),
        modifiers: KeyModifiers::NONE
    })));
}
#[test]
fn workflow_rail_and_workspace_reflow_without_losing_controls() {
    let (mut state, _) = state();
    for (w, h) in [(120, 38), (80, 30), (40, 24)] {
        let rows = draw(&mut state, w, h);
        assert!(
            rows[usize::from(h - 2)].contains("Release readiness"),
            "workflow belongs on the last optional rail: {rows:?}"
        );
        state.open_workflows();
        let text = draw(&mut state, w, h).join("\n");
        for expected in [
            "Release readiness",
            "Working",
            "P Pause",
            "X Stop",
            "Esc Close",
        ] {
            assert!(
                text.contains(expected),
                "{w}x{h} missing {expected}:\n{text}"
            );
        }
        assert!(!text.contains("task-0"));
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    }
}
#[test]
fn workflow_mouse_and_keyboard_preserve_phase_agent_and_scroll() {
    let (mut state, _) = state();
    let rows = draw(&mut state, 120, 38);
    click(&mut state, &rows, "Release readiness");
    assert_eq!(state.workflow_console().view, WorkflowView::Workspace);
    let rows = draw(&mut state, 120, 38);
    click(&mut state, &rows, "Research performance");
    assert_eq!(state.workflow_console().agent().unwrap().task_id, "task-1");
    draw(&mut state, 80, 30);
    press(&mut state, KeyCode::PageDown, KeyModifiers::NONE);
    let scroll = state.workflow_console().scroll();
    assert!(scroll > 0);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.workflow_console().view, WorkflowView::Agent);
    assert_eq!(state.workflow_console().scroll(), scroll);
    assert_eq!(state.workflow_console().phase().unwrap().id, "phase-1");
}
#[test]
fn workflow_typing_does_not_modify_parent_draft() {
    let (mut state, _) = state();
    for c in "PARENT DRAFT".chars() {
        press(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press(&mut state, KeyCode::Char('w'), KeyModifiers::ALT);
    for c in "hello".chars() {
        press(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        draw(&mut state, 120, 38)
            .join("\n")
            .contains("PARENT DRAFT")
    );
}
#[test]
fn workflow_picker_keeps_per_run_selection() {
    let (mut state, source) = state();
    source.rows.lock().unwrap().push(run("run-b"));
    state.refresh_workflows();
    state.open_workflows();
    press(&mut state, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(state.workflow_console().phase().unwrap().id, "phase-2");
    press(&mut state, KeyCode::Char('w'), KeyModifiers::NONE);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.workflow_console().selected().unwrap().id, "run-b");
    assert_eq!(state.workflow_console().phase().unwrap().id, "phase-1");
    press(&mut state, KeyCode::Char('w'), KeyModifiers::NONE);
    press(&mut state, KeyCode::Up, KeyModifiers::NONE);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.workflow_console().phase().unwrap().id, "phase-2");
}
#[test]
fn workflow_flat_view_exposes_actual_selection_and_no_parent_transcript() {
    let (mut state, _) = state();
    state.apply(&heycode_agent::UiEvent::AssistantDelta {
        text: "PARENT TRANSCRIPT".into(),
    });
    state.open_workflows();
    let flat = ScreenReaderSnapshot::from_state(&state).into_text();
    for expected in [
        "Release readiness",
        "Set the scope · Done",
        "Research reliability · Working",
        "P pauses",
    ] {
        assert!(flat.contains(expected), "{flat}");
    }
    assert!(!flat.contains("PARENT TRANSCRIPT"));
}
#[test]
fn workflow_failed_progress_never_becomes_success() {
    let mut run = run("run-a");
    run.status = WorkflowStatus::Failed;
    run.phases[1].status = WorkflowStatus::Failed;
    assert_eq!(run.progress(), (1, 3));
    run.phases[1].nodes[0].status = WorkflowStatus::Failed;
    assert_eq!(run.phases[1].progress(), (0, 1));
}
#[test]
fn workflow_read_failure_removes_stale_progress() {
    let (mut state, source) = state();
    state.open_workflows();
    *source.fail.lock().unwrap() = true;
    state.refresh_workflows();
    assert!(state.workflow_console().runs.is_empty());
    assert!(
        state
            .workflow_console()
            .notice
            .as_ref()
            .unwrap()
            .contains("unavailable")
    );
}
#[tokio::test]
async fn workflow_controls_target_exact_owner_and_never_optimistically_complete() {
    let (mut state, source) = state();
    state.open_workflows();
    press(&mut state, KeyCode::Char('p'), KeyModifiers::NONE);
    state.refresh_workflows();
    tokio::task::yield_now().await;
    state.refresh_workflows();
    assert_eq!(
        source.actions.lock().unwrap().as_slice(),
        &[WorkflowAction {
            run_id: "run-a".into(),
            kind: WorkflowActionKind::Pause
        }]
    );
    assert_eq!(
        state.workflow_console().selected().unwrap().status,
        WorkflowStatus::Running
    );
    press(&mut state, KeyCode::Char('r'), KeyModifiers::NONE);
    state.refresh_workflows();
    assert_eq!(source.actions.lock().unwrap().len(), 1);
}

#[test]
fn workflow_down_enter_escape_restores_composer_cursor_and_typing() {
    let (mut state, _) = state();
    state.input.insert_str("parent draft");
    state.input.move_cursor(tui_textarea::CursorMove::Head);
    let cursor = state.input.cursor();
    press(&mut state, KeyCode::Down, KeyModifiers::NONE);
    assert!(state.workflow_console().rail_focused);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.workflow_console().view, WorkflowView::Workspace);
    press(&mut state, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.workflow_console().agent().unwrap().task_id, "task-1");
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!state.workflow_console().rail_focused);
    assert_eq!(state.input.cursor(), cursor);
    press(&mut state, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(state.input.lines(), &["Xparent draft"]);
}
#[test]
fn workflow_denominator_includes_queued_agent_nodes_and_excludes_retry_rows() {
    let mut run = run("run");
    run.phases[1].nodes = (0..3)
        .map(|i| WorkflowNode {
            id: format!("n{i}"),
            kind: "agent".into(),
            status: if i == 0 {
                WorkflowStatus::Completed
            } else {
                WorkflowStatus::Queued
            },
            ..Default::default()
        })
        .collect();
    let duplicate = run.phases[1].agents[0].clone();
    run.phases[1].agents.push(duplicate);
    assert_eq!(run.agent_progress(), (1, 3));
    run.phases[1].nodes[1].status = WorkflowStatus::Failed;
    assert_eq!(run.agent_progress(), (1, 3));
}
#[test]
fn workflow_approval_preempts_workspace_controls() {
    let (mut state, source) = state();
    state.open_workflows();
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 71,
        name: "write".into(),
        args_preview: "path: guarded.txt".into(),
    });
    press(&mut state, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(source.actions.lock().unwrap().is_empty());
    assert!(state.pending_ask.is_some());
    let full = draw(&mut state, 80, 30).join("\n");
    assert!(full.contains("Create file"), "{full}");
    let flat = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("Permission requested"), "{flat}");
}
#[test]
fn workflow_quit_remains_reachable_in_agent_and_picker() {
    for agent in [false, true] {
        let (mut state, _) = state();
        state.open_workflows();
        if agent {
            press(&mut state, KeyCode::Tab, KeyModifiers::NONE);
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        } else {
            press(&mut state, KeyCode::Char('w'), KeyModifiers::NONE);
        }
        press(&mut state, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))));
    }
}
#[test]
fn workflow_toggle_respects_persisted_binding_and_flat_hint() {
    let (mut state, _) = state();
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        heycode_ui::keymap::KeymapAction::ToggleWorkflows,
        heycode_ui::keymap::KeyChord::parse("alt+g").unwrap(),
    );
    state.set_keymap(heycode_ui::keymap::Keymap::resolve(&overrides).unwrap());
    press(&mut state, KeyCode::Char('w'), KeyModifiers::ALT);
    assert!(!state.workflow_console().expanded());
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .into_text()
            .contains("Alt+G")
    );
    press(&mut state, KeyCode::Char('g'), KeyModifiers::ALT);
    assert!(state.workflow_console().expanded());
}
#[test]
fn workflow_main_tool_row_omits_definition_and_execution_ids() {
    let (mut state, _) = state();
    state.apply(&heycode_agent::UiEvent::ToolStarted{name:"workflow".into(),args:serde_json::json!({"action":"start","definition":{"name":"release","title":"Release readiness","steps":[{"id":"private-node-id"}]}})});
    state.apply(&heycode_agent::UiEvent::ToolFinished {
        name: "workflow".into(),
        ok: true,
        value: serde_json::json!({"run_id":"private-run-id","job_id":"private-job-id"}),
        untrusted_content: None,
    });
    for text in [
        draw(&mut state, 120, 38).join("\n"),
        ScreenReaderSnapshot::from_state(&state).into_text(),
    ] {
        assert!(text.contains("Workflow(Release readiness)"), "{text}");
        assert!(text.contains("Started in background"), "{text}");
        assert!(!text.contains("Completed in"), "{text}");
        for forbidden in [
            "private-node-id",
            "private-run-id",
            "private-job-id",
            "steps",
        ] {
            assert!(!text.contains(forbidden), "{text}");
        }
    }
}

#[test]
fn workflow_paste_cannot_change_hidden_parent_composer() {
    let (mut state, _) = state();
    state.input.insert_str("parent draft");
    state.open_workflows();
    assert!(!state.handle_terminal_event(&Event::Paste("accidental paste".into())));
    assert_eq!(state.input.lines(), &["parent draft"]);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE);
    assert!(!state.handle_terminal_event(&Event::Paste("accidental paste".into())));
    assert_eq!(state.input.lines(), &["parent draft"]);
}

#[test]
fn workflow_history_notice_uses_durable_attempt_outcome_without_live_job_registry() {
    use heycode_session::{
        CURRENT_SESSION_LOG_VERSION, InboxDelivery, InboxMessage, InboxMessageId, InboxSource,
        InboxTarget, SessionEvent, SessionEventKind,
    };
    let (mut state, source) = state();
    source.rows.lock().unwrap()[0].jobs = vec![
        WorkflowJob {
            id: "job-1".into(),
            routine_notice: true,
        },
        WorkflowJob {
            id: "job-2".into(),
            routine_notice: false,
        },
    ];
    state.refresh_workflows();
    let mut seq = 0;
    for (job, visible) in [("job-1", false), ("job-2", true), ("job-3", true)] {
        let text = format!("notice from {job}");
        for kind in [
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: None,
                inserted: vec![
                    InboxMessage::with_source(
                        InboxMessageId::new(format!("notice-{job}")).unwrap(),
                        InboxDelivery::Inject,
                        &text,
                        InboxSource::Job { job_id: job.into() },
                    )
                    .unwrap(),
                ],
                outcome: None,
            },
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: Some(1),
                inserted: vec![],
                outcome: None,
            },
            SessionEventKind::UserMessage { text: text.clone() },
        ] {
            seq += 1;
            state.apply_session_event(&SessionEvent {
                v: CURRENT_SESSION_LOG_VERSION,
                seq,
                time_ms: 1,
                kind,
            });
        }
        assert_eq!(
            ScreenReaderSnapshot::from_state(&state)
                .into_text()
                .contains(&text),
            visible
        );
    }
    seq += 1;
    state.apply_session_event(&SessionEvent {
        v: CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: 1,
        kind: SessionEventKind::UserMessage {
            text: "notice from job-1".into(),
        },
    });
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .into_text()
            .contains("notice from job-1"),
        "The same text typed by a human stays visible"
    );
}

#[test]
fn workflow_failed_tool_call_stays_visible_in_both_renderers() {
    let (mut state, _) = state();
    state.apply(&heycode_agent::UiEvent::ToolStarted {
        name: "workflow".into(),
        args: serde_json::json!({"action":"start","definition":{"title":"Invalid review"}}),
    });
    state.apply(&heycode_agent::UiEvent::ToolFinished {
        name: "workflow".into(),
        ok: false,
        value: serde_json::json!("Invalid declared capability"),
        untrusted_content: None,
    });
    for text in [
        draw(&mut state, 120, 38).join("\n"),
        ScreenReaderSnapshot::from_state(&state).into_text(),
    ] {
        assert!(
            text.contains("Workflow(Invalid review)") && text.contains("Failed"),
            "{text}"
        );
    }
}

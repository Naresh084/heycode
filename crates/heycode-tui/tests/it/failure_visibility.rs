//! Terminal failure evidence is independent of progress prose and active membership.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use heycode_tui::{
    app::{AppState, accessibility::ScreenReaderSnapshot},
    task_console::*,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn diagnostic(id: &str) -> heycode_agent::TaskDiagnostic {
    heycode_agent::TaskDiagnostic {
        id: id.into(),
        message: "Quota exceeded; no capacity available".into(),
        code: Some("rate_limit".into()),
        stage: Some("provider_request".into()),
        run_id: Some("run-42".into()),
        log_location: Some("/retained/diagnostics/run-42.json".into()),
        ..Default::default()
    }
}
fn record(status: TaskStatus) -> TaskRecord {
    TaskRecord {
        key: TaskKey("child:tools".into()),
        kind: TaskKind::Child,
        label: "Tools and execution".into(),
        status,
        parent: Some("parent".into()),
        session: Some("child-session".into()),
        job: None,
        capabilities: TaskCapabilities {
            output: true,
            ..Default::default()
        },
        telemetry: TaskTelemetry {
            elapsed_ms: Some(45_000),
            terminal_diagnostic: (status == TaskStatus::Failed).then(|| diagnostic("failure-1")),
            ..Default::default()
        },
        detail: None,
    }
}
#[derive(Default)]
struct Source {
    rows: Mutex<Vec<TaskRecord>>,
    page: Mutex<TaskOutputPage>,
    unavailable: Mutex<bool>,
    acknowledgements: Mutex<Vec<String>>,
}
#[async_trait]
impl TaskSource for Source {
    fn snapshot(&self) -> Result<Vec<TaskRecord>, String> {
        Ok(self.rows.lock().unwrap().clone())
    }
    fn output(&self, _: &TaskKey, _: Option<u64>, _: usize) -> Result<TaskOutputPage, String> {
        if *self.unavailable.lock().unwrap() {
            Err("event observer unavailable".into())
        } else {
            Ok(self.page.lock().unwrap().clone())
        }
    }
    fn acknowledge_issue(&self, _: &TaskKey, id: &str) -> Result<(), String> {
        self.acknowledgements.lock().unwrap().push(id.into());
        Ok(())
    }
    async fn execute(&self, _: TaskAction, _: CancellationToken) -> Result<String, String> {
        panic!("reviewing failure must not execute or cancel work")
    }
}
fn fixture(status: TaskStatus) -> (AppState, Arc<Source>) {
    let source = Arc::new(Source::default());
    source.rows.lock().unwrap().push(record(status));
    let mut state = AppState::new("fixture", "/workspace".into());
    state.set_task_source(source.clone());
    (state, source)
}
fn screen(state: &mut AppState, width: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
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
fn recovered_tool_error_is_completed_and_settled_failures_are_not_running() {
    let mut completed = record(TaskStatus::Completed);
    completed.telemetry.tool_errors = 3;
    assert_eq!(completed.status_label(), "completed");
    assert!(!completed.active_child());
    for status in [
        TaskStatus::Failed,
        TaskStatus::Cancelled,
        TaskStatus::Interrupted,
        TaskStatus::Completed,
        TaskStatus::Idle,
    ] {
        assert!(!record(status).active_child());
    }
    let (mut state, source) = fixture(TaskStatus::Completed);
    source.rows.lock().unwrap()[0].telemetry.tool_errors = 3;
    state.refresh_tasks();
    assert_eq!(state.task_console().pending_issue_count(), 0);
    assert!(!screen(&mut state, 60).contains("1 agent"));
}
#[test]
fn failed_startup_without_assistant_text_has_readable_typed_error() {
    let (mut state, source) = fixture(TaskStatus::Failed);
    source
        .page
        .lock()
        .unwrap()
        .merge_terminal_diagnostic(diagnostic("failure-1"), 128);
    let footer = screen(&mut state, 60);
    assert!(footer.contains("1 issue"), "{footer}");
    assert!(!footer.contains("1 agent"));
    state.open_task(TaskKey("child:tools".into()));
    let output = screen(&mut state, 110);
    assert!(output.contains("Quota exceeded"), "{output}");
    assert!(output.contains("rate_limit"));
    assert!(output.contains("run-42"));
    assert!(!output.contains("No output yet"));
    assert_eq!(state.task_console().pending_issue_count(), 0);
    let accessible = ScreenReaderSnapshot::from_state(&state);
    assert!(accessible.as_text().contains("Quota exceeded"));
}
#[test]
fn authoritative_diagnostic_survives_unavailable_observer_and_review_preserves_evidence() {
    let (mut state, source) = fixture(TaskStatus::Failed);
    *source.unavailable.lock().unwrap() = true;
    state.open_task(TaskKey("child:tools".into()));
    let output = state.task_console().output_lines().join("\n");
    assert!(output.contains("Quota exceeded"));
    assert!(output.contains("event observer unavailable"));
    // Opening the failure has already reviewed attention without deleting evidence.
    state.refresh_tasks();
    assert_eq!(state.task_console().pending_issue_count(), 0);
    assert!(
        state
            .task_console()
            .output_lines()
            .join("\n")
            .contains("/retained/diagnostics/run-42.json")
    );
    source.rows.lock().unwrap()[0].telemetry.terminal_diagnostic = Some(diagnostic("failure-2"));
    state.refresh_tasks();
    assert_eq!(
        state.task_console().pending_issue_count(),
        1,
        "new run failure is a new issue"
    );
}
#[test]
fn merging_terminal_error_keeps_partial_result_and_deduplicates_by_identity() {
    let mut page = TaskOutputPage {
        events: vec![TaskOutputEvent {
            sequence: 7,
            kind: TaskOutputKind::Text("Partial architecture notes".into()),
        }],
        ..Default::default()
    };
    page.merge_terminal_diagnostic(diagnostic("failure-1"), 128);
    page.merge_terminal_diagnostic(diagnostic("failure-1"), 128);
    assert_eq!(page.events.len(), 2);
    assert!(
        matches!(&page.events[0].kind,TaskOutputKind::Text(text) if text=="Partial architecture notes")
    );
    page.merge_terminal_diagnostic(diagnostic("failure-2"), 128);
    assert_eq!(
        page.events.len(),
        3,
        "matching prose does not merge different failures"
    );
}
#[test]
fn ordinary_status_prose_is_not_promoted_to_a_failure() {
    let (mut state, source) = fixture(TaskStatus::Completed);
    source.page.lock().unwrap().events = vec![TaskOutputEvent {
        sequence: 1,
        kind: TaskOutputKind::Status("Example: fail fast or interrupt safely".into()),
    }];
    state.open_task(TaskKey("child:tools".into()));
    assert!(!screen(&mut state, 110).contains("Example: fail fast"));
    assert_eq!(state.task_console().pending_issue_count(), 0);
}

#[test]
fn missing_diagnostics_are_stated_without_an_empty_successful_transcript() {
    let (mut state, source) = fixture(TaskStatus::Failed);
    source.rows.lock().unwrap()[0].telemetry.terminal_diagnostic = None;
    state.refresh_tasks();
    state.open_task(TaskKey("child:tools".into()));
    let output = state.task_console().output_lines().join("\n");
    assert!(output.contains("Failure diagnostics were not provided"));
    assert!(output.contains("Retained log location was not provided"));
    assert!(!output.contains("No output yet"));
}

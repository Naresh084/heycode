//! U01 rendered workspace-trust boundary, keyboard actions, and dispatch blocking.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_onboarding::OnboardingService;
use heycode_trust::{
    ProjectContentPolicy, TrustPersistence, UntrustedProjectAccess, WorkspaceTrustDecision,
    WorkspaceTrustService,
};
use heycode_tui::app::{AppState, PendingAskView, TuiRunOutcome};
use ratatui::{Terminal, backend::TestBackend};

fn strict() -> ProjectContentPolicy {
    ProjectContentPolicy::new(UntrustedProjectAccess::Block, UntrustedProjectAccess::Block)
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn ctrl(code: char) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code: crossterm::event::KeyCode::Char(code),
        modifiers: crossterm::event::KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
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

fn fixture() -> (tempfile::TempDir, WorkspaceTrustService, AppState) {
    let root = tempfile::tempdir().unwrap();
    let service = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    let prompt = service.dialog_prompt().unwrap();
    let mut state = AppState::new("model", root.path().to_path_buf());
    state.receive_workspace_trust(prompt);
    (root, service, state)
}

#[test]
fn trust_modal_after_connection_renders_only_typed_facts() {
    let (root, _service, mut state) = fixture();
    state.set_onboarding(Arc::new(OnboardingService::new(false)));
    state.pending_ask = Some(PendingAskView::new(1, "private-tool-name", "private-args"));
    let text = frame(&mut state);
    let canonical = root.path().canonicalize().unwrap();
    assert!(text.contains("Trust this workspace?"), "{text}");
    assert!(
        text.contains(canonical.to_string_lossy().as_ref()),
        "{text}"
    );
    assert!(text.contains("Trust once"), "{text}");
    assert!(text.contains("Trust this workspace"), "{text}");
    assert!(text.contains("Open read-only"), "{text}");
    assert!(text.contains("Exit"), "{text}");
    assert!(text.contains("project plugins / MCP / hooks"), "{text}");
    assert!(!text.contains("Welcome to heycode"), "{text}");
    assert!(!text.contains("private-tool-name"), "{text}");
    assert!(!text.contains("private-args"), "{text}");
    assert!(!text.contains("Ask anything"), "{text}");
    assert_eq!(state.workspace_trust().unwrap().selected(), 2);
}

#[test]
fn trust_once_commits_through_live_service_and_requests_typed_recomposition() {
    let (_root, service, mut state) = fixture();
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(state.workspace_trust().unwrap().selected(), 0);
    assert!(!state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter)));

    assert_eq!(
        state.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeWorkspaceTrust {
            decision: WorkspaceTrustDecision::Trusted,
            persistence: TrustPersistence::Session,
            revision: 1,
        })
    );
    assert_eq!(
        service.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Trusted
    );
    assert!(state.workspace_trust().is_none());
    assert!(state.pending_send.is_none());
}

#[test]
fn persistent_and_exit_actions_have_distinct_typed_outcomes() {
    let (_root, service, mut persistent) = fixture();
    persistent.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(persistent.workspace_trust().unwrap().selected(), 1);
    persistent.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        persistent.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeWorkspaceTrust {
            decision: WorkspaceTrustDecision::Trusted,
            persistence: TrustPersistence::Persistent,
            revision: 1,
        })
    );
    assert_eq!(
        service.snapshot().unwrap().persistence(),
        TrustPersistence::Persistent
    );

    let (_root, exit_service, mut exit) = fixture();
    assert!(exit.handle_terminal_event(&key(crossterm::event::KeyCode::Esc)));
    assert_eq!(exit.take_run_outcome(), Some(TuiRunOutcome::Exit));
    assert_eq!(
        exit_service.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
}

#[test]
fn unresolved_trust_consumes_composer_palette_slash_and_paste_events() {
    let (_root, service, mut state) = fixture();
    let _ = state.input.insert_str("/help private-input");
    state.handle_terminal_event(&ctrl('p'));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('/')));
    state.handle_terminal_event(&crossterm::event::Event::Paste("private-paste".to_owned()));
    assert!(state.command_palette().is_none());
    assert!(state.pending_send.is_none());
    assert_eq!(state.input.lines().join("\n"), "/help private-input");
    let rendered = frame(&mut state);
    assert!(!rendered.contains("private-input"), "{rendered}");
    assert!(!rendered.contains("private-paste"), "{rendered}");

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.pending_send.is_none());
    assert_eq!(
        service.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Restricted
    );
    assert!(matches!(
        state.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeWorkspaceTrust {
            decision: WorkspaceTrustDecision::Restricted,
            ..
        })
    ));
}

#[test]
fn stale_prompt_refreshes_authoritative_state_and_double_ctrl_c_exits() {
    let (_root, service, mut state) = fixture();
    service
        .set_session(WorkspaceTrustDecision::Trusted, 0)
        .unwrap();
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeWorkspaceTrust {
            decision: WorkspaceTrustDecision::Trusted,
            persistence: TrustPersistence::Session,
            revision: 1,
        })
    );

    let (_root, service, mut state) = fixture();
    assert!(!state.handle_terminal_event(&ctrl('c')));
    assert!(state.handle_terminal_event(&ctrl('c')));
    assert_eq!(state.take_run_outcome(), Some(TuiRunOutcome::Exit));
    assert_eq!(
        service.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
}

#[test]
fn connection_precedes_trust_without_granting_project_access() {
    let (_root, _service, mut state) = fixture();
    state.set_onboarding(Arc::new(OnboardingService::new(true)));
    let text = frame(&mut state);
    assert!(text.contains("Welcome to heycode"), "{text}");
    assert!(!text.contains("Trust this workspace?"), "{text}");
    assert!(state.workspace_trust().is_some());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.onboarding_outcome,
        Some(heycode_onboarding::OnboardingOutcome::RuntimeClassSelected(
            heycode_onboarding::RuntimeClass::Subscription
        ))
    );
    assert!(state.take_run_outcome().is_none());
    assert!(state.workspace_trust().is_some());
    let flat = heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state);
    assert!(flat.as_text().contains("Welcome to heycode"));
}

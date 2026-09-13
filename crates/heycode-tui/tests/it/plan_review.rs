//! The production reducer/renderer and typed review waiter share one exact decision.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use heycode_agent::{ApprovalPolicy, InteractiveApproval, PlanReviewDecision, UiEvent};
use heycode_tui::{app::AppState, render::draw};
use ratatui::{Terminal, backend::TestBackend};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn state() -> AppState {
    AppState::new("fixture-model", std::env::temp_dir())
}
fn frame(state: &mut AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn long_markdown_is_complete_scrollable_and_controls_cannot_accept_accidentally() {
    let mut state = state();
    let document = format!(
        "# Full detailed proposal\n\n## Objective\nKeep every section\n\n{}\n## Final validation\nUNIQUE-END-OF-FULL-PLAN",
        (0..1500)
            .map(|i| format!("- Implementation step {i}: preserve and validate the result.\n"))
            .collect::<String>()
    );
    state.apply(&UiEvent::PlanReviewRequested {
        id: 3,
        plan: document.clone(),
    });
    let first = frame(&mut state, 90, 30);
    assert!(first.contains("Full detailed proposal"), "{first}");
    assert!(
        first.contains("Accepted edits")
            && first.contains("Default permissions")
            && first.contains("stay in Plan")
    );
    assert_eq!(state.pending_plan_review.as_ref().unwrap().plan, document);
    state.handle_terminal_event(&key(KeyCode::End));
    let last = frame(&mut state, 90, 30);
    assert!(last.contains("UNIQUE-END-OF-FULL-PLAN"), "{last}");
    state.handle_terminal_event(&key(KeyCode::Up));
    let before = state.pending_plan_review.as_ref().unwrap().scroll;
    state.handle_terminal_event(&key(KeyCode::PageUp));
    assert!(state.pending_plan_review.as_ref().unwrap().scroll < before);
    state.handle_terminal_event(&key(KeyCode::Home));
    assert!(frame(&mut state, 64, 22).contains("Full detailed proposal"));
    state.handle_terminal_event(&key(KeyCode::Char('y')));
    assert_eq!(
        state.pending_plan_review.as_ref().unwrap().selection,
        2,
        "ordinary approval shortcut must only type feedback"
    );
    assert_eq!(state.pending_plan_review.as_ref().unwrap().feedback, "y");
    assert!(state.pending_send.is_none());
    state.handle_terminal_event(&key(KeyCode::Esc));
    assert!(state.pending_plan_review.is_none());
    assert!(state.pending_send.is_none());
}

#[test]
fn accessible_view_reaches_the_full_plan_tail_and_strips_terminal_controls() {
    let mut state = state();
    let document = format!(
        "# Start\n{}\n# Last section\nTAIL-MARKER\u{1b}]8;;https://example.test\u{7}",
        "A line of plan prose.\n\n".repeat(800)
    );
    state.apply(&UiEvent::PlanReviewRequested {
        id: 1,
        plan: document,
    });
    state.handle_terminal_event(&key(KeyCode::End));
    let flat = heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state);
    let text = flat.as_text();
    assert!(text.contains("TAIL-MARKER"), "{text}");
    assert!(!text.contains('\u{1b}'));
    assert!(
        text.contains("Accepted edits")
            && text.contains("Default permissions")
            && text.contains("stay in Plan")
    );
}

#[tokio::test]
async fn all_three_choices_and_escape_resolve_the_typed_waiter_only() {
    for (navigation, decision) in [
        (Some(KeyCode::Tab), PlanReviewDecision::AcceptedEdits),
        (Some(KeyCode::Left), PlanReviewDecision::DefaultPermissions),
        (
            None,
            PlanReviewDecision::StayInPlan {
                feedback: String::new(),
            },
        ),
        (
            Some(KeyCode::Esc),
            PlanReviewDecision::StayInPlan {
                feedback: String::new(),
            },
        ),
    ] {
        let bus = heycode_core::EventBus::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bus.on::<UiEvent>(move |event| {
            let _sent = tx.send(event.clone());
        });
        let policy = std::sync::Arc::new(InteractiveApproval::new(bus));
        policy.set_plan_review_available(true);
        let requester = policy.clone();
        let pending = tokio::spawn(async move {
            requester
                .review_plan(
                    "# Complete plan\n## Validation\nRun the tests",
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
        });
        let mut state = state();
        state.approvals = Some(policy.clone());
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        state.apply(&event);
        let id = state.pending_plan_review.as_ref().unwrap().id;
        policy.answer(id, true);
        assert!(policy.plan_is_pending(id));
        if let Some(code) = navigation {
            state.handle_terminal_event(&key(code));
        }
        if navigation != Some(KeyCode::Esc) {
            state.handle_terminal_event(&key(KeyCode::Enter));
        }
        let actual = tokio::time::timeout(std::time::Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, decision);
        assert!(!policy.plan_is_pending(id));
        assert!(state.pending_plan_review.is_none());
    }
}

#[tokio::test]
async fn surface_failure_and_cancellation_dismiss_without_authorization() {
    let policy = InteractiveApproval::new(heycode_core::EventBus::default());
    // Merely having an ordinary approval policy is insufficient to wait for plan acceptance.
    assert!(matches!(
        policy
            .review_plan("# Plan", tokio_util::sync::CancellationToken::new())
            .await,
        PlanReviewDecision::StayInPlan { .. }
    ));
    policy.set_plan_review_available(true);
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        policy.review_plan("# Plan", cancellation).await,
        PlanReviewDecision::StayInPlan { .. }
    ));
}

//! Approval-dialog contract: render card, arrow/enter routing, answer flow.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_agent::{ApprovalPolicy as _, InteractiveApproval, UiEvent};
use heycode_core::EventBus;
use heycode_tools::{ToolCallInput, Verdict};
use heycode_tui::app::accessibility::ScreenReaderSnapshot;
use heycode_tui::app::{AppState, Item};
use ratatui::{Terminal, backend::TestBackend};

fn frame_text(state: &mut AppState, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal_draw(&mut term, state);
    let buffer = term.backend().buffer().clone();
    buffer
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .join("")
}

fn terminal_draw(terminal: &mut Terminal<TestBackend>, state: &mut AppState) {
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    use crossterm::event::{KeyEvent, KeyEventState, KeyModifiers};
    crossterm::event::Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn call(name: &str) -> ToolCallInput {
    ToolCallInput {
        name: name.to_owned(),
        args: serde_json::json!({"path": "a"}),
    }
}

#[test]
fn approval_request_renders_the_dialog_card() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "bash".into(),
        args_preview: "command: rm -rf /tmp/x".into(),
    });
    let text = frame_text(&mut state, 70, 18);
    assert!(!text.contains("Permission requested"), "{text}");
    assert!(text.contains("Esc to cancel · Tab to amend"), "{text}");
    assert!(text.contains("Bash command"), "{text}");
    assert!(text.contains("Do you want to proceed?"), "{text}");
    assert!(text.contains("1. Yes"), "{text}");
    assert!(text.contains("2. No"), "{text}");
}

fn state_with_complete_config_read() -> AppState {
    const REVISION: &str = "68e51be1877f35c23c31de14f66f805b4c87ce666d15fdb7556407ba89ade051";
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&UiEvent::ToolStarted {
        name: "read".into(),
        args: serde_json::json!({"path":"src/config.txt"}),
    });
    state.apply(&UiEvent::ToolFinished {
        name: "read".into(),
        ok: true,
        value: serde_json::json!({
            "path": "src/config.txt",
            "content": "   1\talpha\n   2\tmode=slow\n   3\tgamma",
            "offset": 1,
            "lines_returned": 3,
            "lines_remaining": 0,
            "total_lines": 3,
            "total_bytes": 22,
            "bytes_returned": 22,
            "revision": REVISION,
            "page_line_ending": "lf",
            "truncated": false,
            "next_offset": null,
            "next_byte_offset": null,
            "partial_last_line": false,
            "scan_limited": false,
            "continuation": null
        }),
        untrusted_content: None,
    });
    state
}

fn config_edit_args() -> serde_json::Value {
    serde_json::json!({
        "path": "src/config.txt",
        "old_string": "mode=slow",
        "new_string": "mode=fast",
        "expected_revision": "68e51be1877f35c23c31de14f66f805b4c87ce666d15fdb7556407ba89ade051"
    })
}

#[test]
fn native_edit_approval_uses_the_exact_retained_read_for_a_structured_diff() {
    let mut state = state_with_complete_config_read();
    state.apply(&UiEvent::ToolStarted {
        name: "edit".into(),
        args: config_edit_args(),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 7,
        name: "edit".into(),
        args_preview: "path: src/config.txt\nexpected_revision: hidden\nnew_string: mode=fast\nold_string: mode=slow".into(),
    });

    let text = frame_text(&mut state, 100, 30);
    for expected in [
        "Edit file",
        "src/config.txt",
        "1  alpha",
        "2 -mode=slow",
        "2 +mode=fast",
        "3  gamma",
        "Do you want to make this edit to config.txt?",
        "1. Yes",
        "No",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    assert!(!text.contains("expected_revision:"), "{text}");
    assert!(!text.contains("old_string:"), "{text}");
    assert!(!text.contains("new_string:"), "{text}");

    let accessible = ScreenReaderSnapshot::from_state(&state);
    let accessible = accessible.as_text();
    assert!(accessible.contains("tool: Edit file"), "{accessible}");
    assert!(
        accessible.contains("old line 2 removed: mode=slow"),
        "{accessible}"
    );
    assert!(
        accessible.contains("new line 2 added: mode=fast"),
        "{accessible}"
    );
    assert!(!accessible.contains("expected_revision:"), "{accessible}");
}

#[test]
fn edit_approval_without_an_exact_complete_read_keeps_the_raw_accessible_fallback() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&UiEvent::ToolStarted {
        name: "edit".into(),
        args: config_edit_args(),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 9,
        name: "edit".into(),
        args_preview: "path: src/config.txt\nold_string: mode=slow\nnew_string: mode=fast".into(),
    });

    let visual = frame_text(&mut state, 100, 24);
    assert!(visual.contains("old_string: mode=slow"), "{visual}");
    // The heading still names the kind of action; what must be absent is a
    // structured diff nothing in the transcript proves.
    assert!(!visual.contains("-mode=slow"), "{visual}");
    let accessible = ScreenReaderSnapshot::from_state(&state);
    let accessible = accessible.as_text();
    assert!(accessible.contains("tool: edit"), "{accessible}");
    assert!(accessible.contains("old_string: mode=slow"), "{accessible}");
    assert!(!accessible.contains("tool: Edit file"), "{accessible}");
}

#[test]
fn approval_arriving_before_the_edit_call_gets_the_same_source_backed_preview() {
    let mut state = state_with_complete_config_read();
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 8,
        name: "edit".into(),
        args_preview: "path: src/config.txt\nold_string: mode=slow\nnew_string: mode=fast".into(),
    });
    assert!(frame_text(&mut state, 100, 30).contains("old_string: mode=slow"));

    state.apply(&UiEvent::ToolStarted {
        name: "edit".into(),
        args: config_edit_args(),
    });
    let text = frame_text(&mut state, 100, 30);
    assert!(text.contains("2 -mode=slow"), "{text}");
    assert!(text.contains("2 +mode=fast"), "{text}");
    assert!(!text.contains("old_string:"), "{text}");
}

#[tokio::test]
async fn enter_allows_and_routes_to_the_policy() {
    let policy = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.approvals = Some(policy.clone());
    state.apply(&UiEvent::ToolStarted {
        name: "write".into(),
        args: serde_json::json!({"path":"a"}),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "write".into(),
        args_preview: "path: a".into(),
    });

    let p = policy.clone();
    let decide = tokio::spawn(async move { p.decide(&call("write")).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));

    assert!(matches!(decide.await.unwrap(), Verdict::Allow));
    assert!(state.pending_ask.is_none(), "dialog closes after answering");
    assert!(
        matches!(state.items.last(), Some(Item::Tool { view, .. }) if view.approval.as_deref() == Some("approved"))
    );
    assert_eq!(state.items.len(), 1, "approval updates the original card");
}

#[tokio::test]
async fn down_moves_selection_then_enter_denies() {
    let policy = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.approvals = Some(policy.clone());
    state.apply(&UiEvent::ToolStarted {
        name: "edit".into(),
        args: serde_json::json!({"path":"b"}),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "edit".into(),
        args_preview: "path: b".into(),
    });

    let p = policy.clone();
    let decide = tokio::spawn(async move { p.decide(&call("edit")).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    let text = frame_text(&mut state, 80, 18);
    assert!(text.contains("❯ 2. No"), "{text}");
    assert!(text.contains("1. Yes"), "{text}");

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));

    match decide.await.unwrap() {
        Verdict::Deny { reason } => assert!(reason.contains("dialog")),
        other => panic!("expected denial, got {other:?}"),
    }
    assert!(
        matches!(state.items.last(), Some(Item::Tool { view, .. }) if view.approval.as_deref() == Some("rejected"))
    );
    assert_eq!(state.items.len(), 1, "rejection updates the original card");
}

#[test]
fn esc_on_dialog_denies_without_touching_interrupt() {
    let policy = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.approvals = Some(policy.clone());
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 1,
        name: "bash".into(),
        args_preview: "".into(),
    });

    // Esc while a dialog is up answers the dialog; interrupt_fn stays armed.
    state.interrupt_fn = Some(Box::new(|| {}));
    let before = state.interrupt_fn.is_some();
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(before && state.interrupt_fn.is_some());
    assert!(state.pending_ask.is_none());
}

#[tokio::test]
async fn parallel_tool_calls_queue_their_dialogs_instead_of_evicting_one() {
    let policy = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.approvals = Some(policy.clone());

    // Two tool calls of one parallel batch park on the same policy.
    let first_policy = policy.clone();
    let first = tokio::spawn(async move { first_policy.decide(&call("write")).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let second_policy = policy.clone();
    let second = tokio::spawn(async move { second_policy.decide(&call("bash")).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "write".into(),
        args_preview: "path: a".into(),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 1,
        name: "bash".into(),
        args_preview: "command: ls".into(),
    });
    assert_eq!(
        state.pending_ask.as_ref().map(|ask| ask.id),
        Some(0),
        "the first dialog keeps the screen while the second waits"
    );
    let text = frame_text(&mut state, 70, 18);
    assert!(text.contains("Create file"), "{text}");
    assert!(
        text.contains("1 more waiting"),
        "the waiting dialog must be visible, not silent: {text}"
    );

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.pending_ask.as_ref().map(|ask| ask.id),
        Some(1),
        "the queued dialog takes the slot once the first is answered"
    );
    let text = frame_text(&mut state, 70, 18);
    assert!(text.contains("Bash command"), "{text}");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));

    assert!(
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), first)
                .await
                .unwrap()
                .unwrap(),
            Verdict::Allow
        ),
        "the first caller must be answered"
    );
    assert!(
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), second)
                .await
                .unwrap()
                .unwrap(),
            Verdict::Allow
        ),
        "the evicted caller must be answered too"
    );
    assert!(state.pending_ask.is_none());
}

#[test]
fn an_externally_resolved_ask_never_reaches_the_screen() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "write".into(),
        args_preview: "path: a".into(),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 1,
        name: "bash".into(),
        args_preview: "command: ls".into(),
    });
    // The policy answered the queued ask itself (cancellation, deny-all).
    state.apply(&UiEvent::ApprovalResolved {
        id: 1,
        allowed: false,
    });
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(
        state.pending_ask.is_none(),
        "an already-resolved ask must not open a stale dialog"
    );
}

/// A native approval reaches the TUI twice — once on the agent's UI bus as
/// `ApprovalRequested`, once through the app-server as the same request
/// re-labelled `approval-<id>`. Only one card may open, and the app-server copy
/// must not be mistaken for a second, delegated-runtime request.
#[test]
fn a_native_approval_mirrored_through_the_app_server_opens_one_card() {
    let mut state =
        heycode_tui::app::AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 7,
        name: "bash".to_owned(),
        args_preview: "command: ls".to_owned(),
    });
    state.apply(&heycode_agent::UiEvent::RuntimePermissionRequested {
        request_id: "approval-7".to_owned(),
        action: "bash".to_owned(),
        detail: "command: ls".to_owned(),
    });
    assert_eq!(state.pending_ask.as_ref().map(|ask| ask.id), Some(7));
    assert_eq!(
        state.queued_ask_count(),
        0,
        "the mirrored copy is not a second ask"
    );

    // A genuinely delegated request still opens.
    state.apply(&heycode_agent::UiEvent::RuntimePermissionRequested {
        request_id: "codex-request-1".to_owned(),
        action: "Codex command execution".to_owned(),
        detail: "Run tests".to_owned(),
    });
    assert_eq!(state.queued_ask_count(), 1);
}

#[test]
fn an_agent_request_bridge_is_deduplicated_only_when_the_direct_approval_bus_is_attached() {
    let bridged = heycode_agent::UiEvent::RuntimePermissionRequested {
        request_id: "agent-request-call-1".to_owned(),
        action: "bash".to_owned(),
        detail: "command: ls".to_owned(),
    };

    let mut tui = AppState::new("model", std::path::PathBuf::from("/workspace"));
    tui.approvals = Some(std::sync::Arc::new(InteractiveApproval::new(
        EventBus::default(),
    )));
    tui.apply(&bridged);
    assert!(
        tui.pending_ask.is_none(),
        "the direct approval bus owns this request"
    );

    let mut app_server_client = AppState::new("model", std::path::PathBuf::from("/workspace"));
    app_server_client.apply(&bridged);
    assert_eq!(
        app_server_client
            .pending_ask
            .as_ref()
            .map(|ask| ask.name.as_str()),
        Some("bash"),
        "clients without the direct approval bus still need the bridge"
    );
}

/// Claude Code's fourth choice: refuse *and say why*, so the model redirects
/// instead of just stopping.
#[tokio::test]
async fn deny_with_reason_collects_one_line_and_sends_it_to_the_model() {
    use crossterm::event::KeyCode;
    let policy = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.approvals = Some(policy.clone());
    state.apply(&UiEvent::ToolStarted {
        name: "bash".into(),
        args: serde_json::json!({"command":"rm -rf /data"}),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "bash".into(),
        args_preview: "command: rm -rf /data".into(),
    });
    let p = policy.clone();
    let decide = tokio::spawn(async move { p.decide(&call("bash")).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let text = frame_text(&mut state, 70, 20);
    assert!(text.contains("No"), "the card offers it: {text}");

    // Tab opens the amendment editor without answering yet; r remains an alias.
    state.handle_terminal_event(&key(KeyCode::Tab));
    assert!(state.pending_ask.is_some(), "no answer was sent yet");
    assert_eq!(
        state.pending_ask.as_ref().and_then(|ask| ask.reason()),
        Some("")
    );
    let text = frame_text(&mut state, 70, 20);
    assert!(text.contains("Amendment"), "{text}");

    // Escape backs out of the editor and leaves the card open — Escape is
    // "back", not "deny", once a layer is on top.
    state.handle_terminal_event(&key(KeyCode::Esc));
    assert!(
        state.pending_ask.is_some(),
        "escape left the editor, not the card"
    );
    assert!(
        state
            .pending_ask
            .as_ref()
            .and_then(|ask| ask.reason())
            .is_none()
    );

    state.handle_terminal_event(&key(KeyCode::Char('r')));
    for character in "use the staging bucketX".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character)));
    }
    state.handle_terminal_event(&key(KeyCode::Backspace));
    assert_eq!(
        state.pending_ask.as_ref().and_then(|ask| ask.reason()),
        Some("use the staging bucket")
    );
    let text = frame_text(&mut state, 70, 20);
    assert!(text.contains("use the staging bucket"), "{text}");

    state.handle_terminal_event(&key(KeyCode::Enter));
    let Verdict::Deny { reason } = decide.await.unwrap() else {
        panic!("a reasoned answer still denies");
    };
    assert_eq!(reason, "denied by the user: use the staging bucket");
    assert!(state.pending_ask.is_none());
    assert!(
        matches!(state.items.last(), Some(Item::Tool { view, .. })
            if view.approval.as_deref() == Some("rejected: use the staging bucket")),
        "{:?}",
        state.items.last()
    );
}

#[tokio::test]
async fn an_empty_reason_is_a_plain_denial_and_typing_never_leaks_into_the_composer() {
    use crossterm::event::KeyCode;
    let policy = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.approvals = Some(policy.clone());
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 0,
        name: "bash".into(),
        args_preview: "command: ls".into(),
    });
    let p = policy.clone();
    let decide = tokio::spawn(async move { p.decide(&call("bash")).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    state.handle_terminal_event(&key(KeyCode::Char('r')));
    for character in "hello".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character)));
    }
    assert_eq!(
        state.input.lines(),
        [""],
        "the card owns the keyboard while it is open"
    );
    for _ in 0..5 {
        state.handle_terminal_event(&key(KeyCode::Backspace));
    }
    state.handle_terminal_event(&key(KeyCode::Enter));
    let Verdict::Deny { reason } = decide.await.unwrap() else {
        panic!("denies");
    };
    assert_eq!(reason, "denied via approval dialog", "no reason, no prefix");
}

#[tokio::test]
async fn accepted_edits_card_grants_future_calls_only_on_explicit_choice() {
    let interactive = std::sync::Arc::new(InteractiveApproval::new(EventBus::default()));
    let accepted = std::sync::Arc::new(heycode_agent::AcceptedEdits::new(interactive.clone()));
    let mut subscription = interactive.take_subscription().unwrap();
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.permission = "accepted_edits".into();
    state.approvals = Some(interactive.clone());
    for remember in [false, true] {
        let task = tokio::spawn({
            let accepted = accepted.clone();
            async move { accepted.decide(&call("mcp__files__edit")).await }
        });
        let event = subscription.recv().await.unwrap();
        state.apply(&UiEvent::ApprovalRequested {
            owner_session: None,
            id: event.id,
            name: event.name,
            args_preview: event.args_preview,
        });
        let text = frame_text(&mut state, 80, 24);
        for expected in [
            "1. Yes",
            "2. Yes, and allow identical calls this session",
            "3. No",
            "Esc to cancel · Tab to amend",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("Deny with reason"));
        if remember {
            state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
            let selected = frame_text(&mut state, 80, 24);
            assert!(
                selected.contains("❯ 2. Yes, and allow identical calls this session"),
                "{selected}"
            );
        }
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
        assert!(matches!(task.await.unwrap(), Verdict::Allow));
    }
    assert!(matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            accepted.decide(&call("mcp__files__edit"))
        )
        .await
        .unwrap(),
        Verdict::Allow
    ));
    let task = tokio::spawn({
        let accepted = accepted.clone();
        async move {
            accepted
                .decide(&ToolCallInput {
                    name: "mcp__files__edit".into(),
                    args: serde_json::json!({"path":"different"}),
                })
                .await
        }
    });
    let event = subscription.recv().await.unwrap();
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: event.id,
        name: event.name,
        args_preview: event.args_preview,
    });
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(matches!(task.await.unwrap(), Verdict::Deny { .. }));
}

#[test]
fn approval_arriving_before_tool_event_still_updates_that_card() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: 91,
        name: "write".into(),
        args_preview: "path: late.txt".into(),
    });
    state.apply(&UiEvent::ToolStarted {
        name: "write".into(),
        args: serde_json::json!({"path":"late.txt"}),
    });
    assert!(
        matches!(state.items.last(), Some(Item::Tool { view, .. }) if view.approval.as_deref() == Some("awaiting approval"))
    );
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('n')));
    state.apply(&UiEvent::ToolFinished {
        name: "write".into(),
        ok: false,
        value: serde_json::json!("denied: denied via approval dialog"),
        untrusted_content: None,
    });
    assert_eq!(state.items.len(), 1);
    assert!(
        matches!(state.items.last(), Some(Item::Tool { view, .. }) if view.approval.as_deref() == Some("rejected"))
    );
    assert!(frame_text(&mut state, 100, 25).contains("denied via approval dialog"));
}

//! A05/U18 native composer delivery and visible inbox contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use heycode_agent::{InboxWake, UiEvent};
use heycode_session::InboxDelivery;
use heycode_tui::ScreenReaderSnapshot;
use heycode_tui::app::AppState;

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn active_state() -> AppState {
    let mut state = AppState::new("model", "/workspace".into());
    state.runtime = "native".to_owned();
    state.apply(&UiEvent::TurnStarted { turn: 1 });
    state
}

fn type_text(state: &mut AppState, text: &str) {
    state.input = tui_textarea::TextArea::default();
    assert!(state.input.insert_str(text));
}

#[test]
fn active_enter_steers_and_tab_queues_follow_up_without_publishing_user_text() {
    let mut steer = active_state();
    type_text(&mut steer, "change direction");
    steer.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        steer.take_inbox_submission(),
        Some((InboxDelivery::Steer, "change direction".to_owned()))
    );
    assert!(steer.input.lines().iter().all(String::is_empty));
    assert!(
        steer.items.is_empty(),
        "submission is not model-visible yet"
    );

    let mut follow_up = active_state();
    type_text(&mut follow_up, "after this, inspect docs");
    follow_up.handle_terminal_event(&key(crossterm::event::KeyCode::Tab));
    assert_eq!(
        follow_up.take_inbox_submission(),
        Some((
            InboxDelivery::FollowUp,
            "after this, inspect docs".to_owned()
        ))
    );
    assert!(follow_up.items.is_empty());
}

#[test]
fn idle_tab_remains_composer_input_and_never_queues_operational_work() {
    let mut state = AppState::new("model", "/workspace".into());
    type_text(&mut state, "draft");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Tab));
    assert!(state.take_inbox_submission().is_none());
    assert!(state.input.lines().join("\n").contains("draft"));
}

#[test]
fn inbox_counts_and_key_semantics_are_visible_and_one_wake_is_consumed_once() {
    let mut state = active_state();
    state.apply(&UiEvent::InboxUpdated {
        next_turn: 2,
        next_step: 1,
        wake: InboxWake::Queued,
    });
    assert_eq!(state.inbox_pending().next_turn, 2);
    assert_eq!(state.inbox_pending().next_step, 1);
    assert!(!state.take_follow_up_wake());

    state.apply(&UiEvent::InboxUpdated {
        next_turn: 2,
        next_step: 0,
        wake: InboxWake::Wake,
    });
    state.set_onboarding(std::sync::Arc::new(
        heycode_onboarding::OnboardingService::new(true),
    ));
    assert!(
        !state.take_follow_up_wake(),
        "setup must not wake retained model input"
    );
    state.set_onboarding(std::sync::Arc::new(
        heycode_onboarding::OnboardingService::new(false),
    ));
    assert!(state.take_follow_up_wake());
    assert!(!state.take_follow_up_wake(), "one wake has one owner");

    let frame = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(frame.contains("inbox: 2 follow-up; 0 steer"), "{frame}");
    assert!(
        frame.contains("keys: Enter steers; Tab queues follow-up; Escape interrupts"),
        "{frame}"
    );
}

#[test]
fn active_escape_interrupts_without_changing_composer_delivery() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    let mut state = active_state();
    type_text(&mut state, "keep this draft");
    state.interrupt_fn = Some(Box::new(move || flag.store(true, Ordering::SeqCst)));

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(interrupted.load(Ordering::SeqCst));
    assert!(state.take_inbox_submission().is_none());
    assert_eq!(state.input.lines(), ["keep this draft"]);
}

#[test]
fn delegated_runtime_never_falls_back_to_the_native_agent_inbox() {
    let mut state = active_state();
    state.runtime = "codex".to_owned();
    type_text(&mut state, "steer delegated turn");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.take_inbox_submission().is_none());
    assert_eq!(state.input.lines(), ["steer delegated turn"]);
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("steering is unavailable for this delegated runtime")
    );

    type_text(&mut state, "follow delegated turn");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Tab));
    assert!(state.take_inbox_submission().is_none());
    assert_eq!(state.input.lines(), ["follow delegated turn"]);
}

/// A pasted block lands in the composer as ONE multi-line draft and nothing is
/// sent until Enter — Claude Code and Codex both enable bracketed paste for
/// exactly this. Before, line 1 became a prompt and the rest became steers.
#[test]
fn a_bracketed_paste_becomes_one_multiline_draft_and_sends_nothing() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.handle_terminal_event(&crossterm::event::Event::Paste(
        "fix this:\r\nfn a() {}\n  b();".to_owned(),
    ));
    assert_eq!(state.input.lines(), ["fix this:", "fn a() {}", "  b();"]);
    assert!(state.pending_send.is_none(), "paste never sends");
    // Enter then sends the whole block as one message.
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.pending_send.as_deref(),
        Some("fix this:\nfn a() {}\n  b();")
    );
}

/// The input area grows with the draft (up to a cap) so every pasted line is
/// visible, instead of a single row hiding all but the current line.
#[test]
fn the_composer_shows_every_line_of_a_multiline_draft() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.handle_terminal_event(&crossterm::event::Event::Paste(
        "line-one\nline-two\nline-three\nline-four".to_owned(),
    ));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, &mut state))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("");
    for line in ["line-one", "line-two", "line-three", "line-four"] {
        assert!(text.contains(line), "{line} must be visible: {text}");
    }
}

fn key_with(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn type_keys(state: &mut AppState, text: &str) {
    for character in text.chars() {
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Char(character)));
    }
}

/// Three ways to insert a line break without sending: Alt+Enter, Shift+Enter
/// (where the terminal reports it) and Claude Code's `\` at the end of a line
/// followed by Enter.
#[test]
fn newlines_can_be_inserted_three_ways_and_none_of_them_sends() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    type_keys(&mut state, "one");
    state.handle_terminal_event(&key_with(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::ALT,
    ));
    type_keys(&mut state, "two");
    state.handle_terminal_event(&key_with(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::SHIFT,
    ));
    type_keys(&mut state, "three\\");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    type_keys(&mut state, "four");
    assert_eq!(state.input.lines(), ["one", "two", "three", "four"]);
    assert!(state.pending_send.is_none());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(state.pending_send.as_deref(), Some("one\ntwo\nthree\nfour"));
}

/// Ctrl+C: with a draft it clears the draft; with an empty composer it arms a
/// visible "press again to exit" and only a second press quits. Any other key
/// disarms it, so a stray Ctrl+C minutes ago cannot make the next one fatal.
#[test]
fn ctrl_c_clears_then_arms_then_quits_and_other_keys_disarm() {
    let ctrl_c = || {
        key_with(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        )
    };
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    type_keys(&mut state, "a draft");
    assert!(
        !state.handle_terminal_event(&ctrl_c()),
        "first press clears, never quits"
    );
    assert_eq!(state.input.lines(), [""]);
    assert!(
        state.quit_hint().is_none(),
        "clearing a draft does not arm quit"
    );

    assert!(!state.handle_terminal_event(&ctrl_c()));
    assert_eq!(state.quit_hint(), Some("press Ctrl+C again to exit"));
    type_keys(&mut state, "x");
    assert!(state.quit_hint().is_none(), "typing disarms the quit");
    state.handle_terminal_event(&ctrl_c()); // clears "x"
    assert!(!state.handle_terminal_event(&ctrl_c()), "arms");
    assert!(state.handle_terminal_event(&ctrl_c()), "second press quits");
}

/// Up and Down recall earlier prompts, and the in-progress draft comes back
/// when the user walks past the newest entry.
#[test]
fn up_and_down_recall_prompt_history_and_restore_the_draft() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    for prompt in ["first prompt", "second prompt"] {
        type_keys(&mut state, prompt);
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
        state.pending_send.take();
    }
    type_keys(&mut state, "draft");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(state.input.lines(), ["second prompt"]);
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(state.input.lines(), ["first prompt"]);
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(state.input.lines(), ["first prompt"], "stops at the oldest");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    assert_eq!(state.input.lines(), ["draft"], "the draft is restored");
}

/// Up reaches the prompts of previous runs, not only this one — the first
/// thing a user wants to repeat is usually the last thing they typed
/// yesterday.
#[test]
fn prompt_history_survives_a_restart_and_new_prompts_join_it() {
    let home = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(heycode_tui::prompt_history::PromptHistoryStore::new(
        home.path().join("history"),
    ));
    store.append("run the tests");

    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.set_prompt_history_store(store.clone());
    // A fresh process: the previous run's prompt is one Up away.
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(state.input.lines(), ["run the tests"]);

    // Anything typed now joins the same durable history.
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    state.handle_terminal_event(&crossterm::event::Event::Paste("ship it".to_owned()));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(state.pending_send.as_deref(), Some("ship it"));
    assert_eq!(
        store.load(),
        ["run the tests".to_owned(), "ship it".to_owned()]
    );

    // And the next process sees both.
    let mut next = AppState::new("m", std::path::PathBuf::from("/p"));
    next.set_prompt_history_store(std::sync::Arc::new(
        heycode_tui::prompt_history::PromptHistoryStore::new(home.path().join("history")),
    ));
    next.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(next.input.lines(), ["ship it"]);
    next.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(next.input.lines(), ["run the tests"]);
}

#[test]
fn multiple_busy_messages_are_recalled_together_with_cursor_at_end() {
    let mut state = active_state();
    for text in ["first\nline", "second", "third"] {
        type_text(&mut state, text);
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    }
    assert!(
        state.items.is_empty(),
        "queued text is not yet transcript text"
    );
    type_text(&mut state, "still editing");
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Up));
    assert_eq!(
        state.input.lines().join("\n"),
        "first\nline\n\nsecond\n\nthird\n\nstill editing"
    );
    assert_eq!(state.input.cursor(), (7, 13));
    assert!(state.take_inbox_submission().is_none());
    type_keys(&mut state, " more");
    assert!(
        state
            .input
            .lines()
            .last()
            .unwrap()
            .ends_with("still editing more")
    );
}

#[test]
fn multiple_busy_submissions_keep_fifo_order_without_overwriting() {
    let mut state = active_state();
    for text in ["one", "two", "three"] {
        type_text(&mut state, text);
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    }
    for expected in ["one", "two", "three"] {
        assert_eq!(
            state.take_inbox_submission(),
            Some((InboxDelivery::Steer, expected.into()))
        );
    }
    assert!(state.take_inbox_submission().is_none());
}

#[test]
fn repeated_active_ctrl_c_interrupts_without_arming_quit_or_discarding_draft() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let mut state = active_state();
    state.interrupt_fn = Some(Box::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    type_text(&mut state, "preserve this");
    let ctrl_c = key_with(
        crossterm::event::KeyCode::Char('c'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    for _ in 0..3 {
        assert!(!state.handle_terminal_event(&ctrl_c));
        assert!(state.quit_hint().is_none());
        assert_eq!(state.input.lines(), ["preserve this"]);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

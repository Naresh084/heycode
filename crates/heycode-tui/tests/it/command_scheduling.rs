//! U11 active-work command scheduling and confirmation contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandAvailability, CommandDescriptor, CommandRegistry, CommandSource, CommandTiming,
    UiEvent,
};
use heycode_tui::app::{AppState, Item};
use heycode_tui::command_scheduling::{CommandDisposition, route_command};
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

struct ScheduledCommand {
    descriptor: CommandDescriptor,
    availability: CommandAvailability,
}

#[async_trait]
impl Command for ScheduledCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        self.availability.clone()
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, _args: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn registry() -> Arc<CommandRegistry> {
    let mut registry = CommandRegistry::new();
    let source = CommandSource::from_plugin("commands").unwrap();
    for (id, timing, availability) in [
        (
            "status",
            CommandTiming::Immediate,
            CommandAvailability::available(),
        ),
        (
            "model",
            CommandTiming::Queued,
            CommandAvailability::available(),
        ),
        (
            "skill",
            CommandTiming::ModelScheduling,
            CommandAvailability::available(),
        ),
        (
            "quit",
            CommandTiming::Interrupting,
            CommandAvailability::available(),
        ),
        (
            "deploy",
            CommandTiming::Interrupting,
            CommandAvailability::unavailable("Connect deployment runtime").unwrap(),
        ),
    ] {
        registry
            .register(Arc::new(ScheduledCommand {
                descriptor: CommandDescriptor::new(
                    id,
                    format!("Test {id}"),
                    Vec::new(),
                    timing,
                    source.clone(),
                )
                .unwrap(),
                availability,
            }))
            .unwrap();
    }
    Arc::new(registry)
}

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
    state.set_commands(registry());
    state.apply(&UiEvent::TurnStarted { turn: 1 });
    state
}

fn submit(state: &mut AppState, text: &str) {
    state.input = tui_textarea::TextArea::default();
    assert!(state.input.insert_str(text));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
}

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
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
fn timing_routes_are_total_for_idle_and_active_work() {
    for timing in [
        CommandTiming::Immediate,
        CommandTiming::Queued,
        CommandTiming::Interrupting,
        CommandTiming::ModelScheduling,
    ] {
        assert_eq!(route_command(timing, false), CommandDisposition::ExecuteNow);
    }
    assert_eq!(
        route_command(CommandTiming::Immediate, true),
        CommandDisposition::ExecuteNow
    );
    assert_eq!(
        route_command(CommandTiming::Queued, true),
        CommandDisposition::Queue
    );
    assert_eq!(
        route_command(CommandTiming::ModelScheduling, true),
        CommandDisposition::Queue
    );
    assert_eq!(
        route_command(CommandTiming::Interrupting, true),
        CommandDisposition::ConfirmInterrupt
    );
}

#[test]
fn active_turn_executes_immediate_queues_other_work_and_refuses_unavailable() {
    let mut immediate = active_state();
    submit(&mut immediate, "/status");
    assert_eq!(immediate.pending_send.as_deref(), Some("/status"));

    let mut queued = active_state();
    submit(&mut queued, "/model provider/private-model");
    submit(&mut queued, "/skill refactor carefully");
    assert!(queued.pending_send.is_none());
    assert_eq!(queued.queued_command_count(), 2);
    let narration = queued
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Info(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(narration.contains("queued /model"), "{narration}");
    assert!(narration.contains("queued /skill"), "{narration}");
    assert!(!narration.contains("private-model"), "{narration}");
    assert!(!narration.contains("refactor carefully"), "{narration}");

    queued.apply(&UiEvent::TurnFinished {
        reason: "stop".to_owned(),
        usage: None,
        context_tokens: None,
    });
    queued.promote_next_queued_command();
    assert_eq!(
        queued.pending_send.as_deref(),
        Some("/model provider/private-model")
    );

    let mut unavailable = active_state();
    submit(&mut unavailable, "/deploy production");
    assert!(unavailable.pending_send.is_none());
    assert_eq!(unavailable.queued_command_count(), 0);
    assert!(unavailable.items.iter().any(|item| matches!(
        item,
        Item::Error(text) if text.contains("Connect deployment runtime")
    )));
    assert_eq!(unavailable.input.lines(), ["/deploy production"]);
}

#[test]
fn interrupting_command_is_cancel_default_then_runs_only_after_settlement() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut state = active_state();
    let flag = interrupted.clone();
    state.interrupt_fn = Some(Box::new(move || flag.store(true, Ordering::SeqCst)));

    submit(&mut state, "/quit");
    assert_eq!(
        state
            .pending_command_confirmation
            .as_ref()
            .unwrap()
            .selection,
        0
    );
    assert_eq!(state.queued_command_count(), 0);
    let frame = frame_text(&mut state);
    assert!(frame.contains("Interrupt active work?"), "{frame}");
    assert!(frame.contains("Cancel"), "{frame}");
    assert!(frame.contains("Interrupt & run"), "{frame}");

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert!(!interrupted.load(Ordering::SeqCst));
    assert!(state.pending_command_confirmation.is_none());
    assert_eq!(state.input.lines(), ["/quit"]);

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Right));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(interrupted.load(Ordering::SeqCst));
    assert_eq!(state.queued_command_count(), 1);
    assert!(state.pending_send.is_none());

    state.apply(&UiEvent::TurnFinished {
        reason: "aborted".to_owned(),
        usage: None,
        context_tokens: None,
    });
    state.promote_next_queued_command();
    assert_eq!(state.pending_send.as_deref(), Some("/quit"));
}

/// Promotion and re-queueing must share one idleness predicate. A queued
/// command is never promoted while the turn is still marked active — otherwise
/// the runner promotes it, the dispatcher re-queues it, and the two spin
/// forever printing "queued … running …" — and it runs exactly once after
/// settlement.
#[test]
fn queued_commands_wait_for_settlement_and_run_exactly_once() {
    let mut state = active_state();
    submit(&mut state, "/model deepseek-v4-pro");
    let queued_before = state
        .items
        .iter()
        .filter(|item| matches!(item, Item::Info(text) if text.starts_with("queued /model")))
        .count();
    assert_eq!(queued_before, 1);

    // The runner sees no live tasks but the turn is still marked active.
    for _ in 0..5 {
        state.promote_next_queued_command();
    }
    assert!(
        state.pending_send.is_none(),
        "nothing is promoted while active"
    );
    assert!(
        !state
            .items
            .iter()
            .any(|item| matches!(item, Item::Info(text) if text.starts_with("running /model"))),
        "no 'running' notice while the turn is active: {:?}",
        state.items
    );

    state.apply(&UiEvent::TurnFinished {
        reason: "stop".to_owned(),
        usage: None,
        context_tokens: None,
    });
    state.promote_next_queued_command();
    assert_eq!(
        state.pending_send.as_deref(),
        Some("/model deepseek-v4-pro")
    );
    state.promote_next_queued_command();
    let running = state
        .items
        .iter()
        .filter(|item| matches!(item, Item::Info(text) if text.starts_with("running /model")))
        .count();
    assert_eq!(running, 1, "promoted exactly once");
}

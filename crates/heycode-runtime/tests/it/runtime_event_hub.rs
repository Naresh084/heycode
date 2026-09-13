//! R02 retention and fan-out contract for the shared delegated event hub.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use futures::StreamExt as _;
use heycode_runtime::{
    RUNTIME_EVENT_HISTORY, RuntimeErrorCode, RuntimeEvent, RuntimeEventHub, RuntimeEventKind,
    RuntimeFinishReason, RuntimeTurnId, normalize_runtime_event_replay,
    normalize_runtime_event_stream,
};

fn turn(index: usize) -> RuntimeTurnId {
    RuntimeTurnId::new(format!("turn-{index}")).unwrap()
}

fn delta(index: usize) -> RuntimeEventKind {
    RuntimeEventKind::CommentaryDelta {
        text: format!("chunk {index}"),
    }
}

/// Collect the retained window of a hub nothing will emit into again.
async fn snapshot(hub: &RuntimeEventHub) -> Vec<RuntimeEvent> {
    hub.close();
    let mut replay = normalize_runtime_event_replay(hub.subscribe());
    let mut events = Vec::new();
    while let Some(event) = replay.next().await {
        events.push(
            event
                .expect("a retained window must stay R02-valid")
                .into_event(),
        );
    }
    events
}

#[tokio::test]
async fn a_session_longer_than_the_retained_window_keeps_accepting_emissions() {
    let hub = RuntimeEventHub::new();
    hub.emit(RuntimeEventKind::SessionReady).unwrap();
    for index in 0..400 {
        let turn = turn(index);
        hub.emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
            .unwrap();
        hub.emit(delta(index)).unwrap();
        hub.emit(RuntimeEventKind::FinalMessage {
            text: format!("answer {index}"),
        })
        .unwrap();
        hub.emit(RuntimeEventKind::TurnFinished {
            turn,
            reason: RuntimeFinishReason::Stop,
        })
        .unwrap();
    }

    let retained = snapshot(&hub).await;
    assert!(
        retained.len() <= RUNTIME_EVENT_HISTORY,
        "retention must stay bounded, kept {}",
        retained.len()
    );
    assert!(matches!(
        retained.first().map(RuntimeEvent::kind),
        Some(RuntimeEventKind::SessionReady)
    ));
    assert!(matches!(
        retained.last().map(RuntimeEvent::kind),
        Some(RuntimeEventKind::TurnFinished { .. })
    ));
}

#[tokio::test]
async fn one_turn_longer_than_the_window_keeps_its_skeleton_and_settles() {
    let hub = RuntimeEventHub::new();
    hub.emit(RuntimeEventKind::SessionReady).unwrap();
    let turn = turn(0);
    hub.emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
        .unwrap();
    for index in 0..(RUNTIME_EVENT_HISTORY * 3) {
        hub.emit(delta(index)).unwrap();
    }
    hub.emit(RuntimeEventKind::FinalMessage {
        text: "complete".to_owned(),
    })
    .unwrap();
    hub.emit(RuntimeEventKind::TurnFinished {
        turn,
        reason: RuntimeFinishReason::Stop,
    })
    .unwrap();

    let retained = snapshot(&hub).await;
    assert!(retained.len() <= RUNTIME_EVENT_HISTORY);
    assert!(matches!(
        retained.first().map(RuntimeEvent::kind),
        Some(RuntimeEventKind::SessionReady)
    ));
    assert!(
        retained
            .iter()
            .any(|event| matches!(event.kind(), RuntimeEventKind::FinalMessage { .. })),
        "the final message a stopped turn depends on is never evictable"
    );
}

#[tokio::test]
async fn settled_tool_calls_are_evicted_with_their_results() {
    let hub = RuntimeEventHub::new();
    hub.emit(RuntimeEventKind::SessionReady).unwrap();
    let turn = turn(0);
    hub.emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
        .unwrap();
    for index in 0..RUNTIME_EVENT_HISTORY {
        let call_id = heycode_core::CallId::from_raw(format!("call-{index}"));
        hub.emit(RuntimeEventKind::ToolCall {
            call_id: call_id.clone(),
            name: "shell".to_owned(),
            arguments: serde_json::json!({}),
        })
        .unwrap();
        hub.emit(RuntimeEventKind::ToolResult {
            call_id,
            result: serde_json::json!({"ok": true}),
            is_error: false,
        })
        .unwrap();
    }
    hub.emit(RuntimeEventKind::FinalMessage {
        text: "complete".to_owned(),
    })
    .unwrap();
    hub.emit(RuntimeEventKind::TurnFinished {
        turn,
        reason: RuntimeFinishReason::Stop,
    })
    .unwrap();

    let retained = snapshot(&hub).await;
    assert!(retained.len() <= RUNTIME_EVENT_HISTORY);
}

#[tokio::test]
async fn a_subscriber_that_attaches_after_trimming_still_starts_at_session_ready() {
    let hub = RuntimeEventHub::new();
    hub.emit(RuntimeEventKind::SessionReady).unwrap();
    let turn = turn(0);
    hub.emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
        .unwrap();
    for index in 0..(RUNTIME_EVENT_HISTORY * 2) {
        hub.emit(delta(index)).unwrap();
    }

    let mut late = normalize_runtime_event_stream(hub.subscribe());
    hub.emit(RuntimeEventKind::FinalMessage {
        text: "complete".to_owned(),
    })
    .unwrap();
    hub.emit(RuntimeEventKind::TurnFinished {
        turn,
        reason: RuntimeFinishReason::Stop,
    })
    .unwrap();

    let first = late.next().await.unwrap().unwrap();
    assert_eq!(first.sequence(), 0);
    assert!(matches!(first.kind(), RuntimeEventKind::SessionReady));
    let mut sequence = 0;
    loop {
        let event = late.next().await.unwrap().unwrap();
        sequence += 1;
        assert_eq!(event.sequence(), sequence);
        if matches!(event.kind(), RuntimeEventKind::TurnFinished { .. }) {
            break;
        }
    }
    assert!(
        sequence < u64::try_from(RUNTIME_EVENT_HISTORY * 2).unwrap(),
        "a late subscriber replays the retained window, not the whole session"
    );
}

#[tokio::test]
async fn an_invalid_emission_fails_at_the_emitting_call_site() {
    let hub = RuntimeEventHub::new();
    hub.emit(RuntimeEventKind::SessionReady).unwrap();
    let turn = turn(0);
    hub.emit(RuntimeEventKind::TurnStarted { turn: turn.clone() })
        .unwrap();
    // A stopped turn with no final message is exactly the R02 violation that
    // used to escape an adapter and poison a distant consumer's stream.
    let error = hub
        .emit(RuntimeEventKind::TurnFinished {
            turn,
            reason: RuntimeFinishReason::Stop,
        })
        .expect_err("a stopped turn without a final message is invalid");
    assert_eq!(error.code(), RuntimeErrorCode::Protocol);
}

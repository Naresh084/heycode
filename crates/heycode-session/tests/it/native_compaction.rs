#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_session::{
    ProjectedInput, Role, SessionEvent, SessionEventKind, derive_messages,
    opaque_compaction_barrier, project_inputs_for_route,
};

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: 2,
        seq,
        time_ms: 1_800_000_000_000,
        kind,
    }
}

fn checkpoint() -> ProviderStateItem {
    ProviderStateItem::new(
        "openai",
        "gpt-5.6",
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({
            "type":"compaction",
            "id":"cmp_1",
            "encrypted_content":"opaque"
        }),
    )
    .unwrap()
}

fn history() -> Vec<SessionEvent> {
    vec![
        event(
            0,
            SessionEventKind::UserMessage {
                text: "old question".to_owned(),
            },
        ),
        event(
            1,
            SessionEventKind::AssistantMessage {
                turn: 0,
                step: 0,
                content: "old answer".to_owned(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
        ),
        event(
            2,
            SessionEventKind::NativeCompactionApplied {
                strategy: "provider-native".to_owned(),
                replaced_upto_seq: 1,
                items: vec![checkpoint()],
                usage: None,
            },
        ),
        event(
            3,
            SessionEventKind::UserMessage {
                text: "recent question".to_owned(),
            },
        ),
    ]
}

#[test]
fn exact_route_replays_native_checkpoint_and_shadows_only_its_prefix() {
    let inputs = project_inputs_for_route(
        &history(),
        "openai",
        "gpt-5.6",
        ProviderProtocol::OpenAiResponses,
    )
    .unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(matches!(
        &inputs[0],
        ProjectedInput::ProviderState(item) if item == &checkpoint()
    ));
    assert!(matches!(
        &inputs[1],
        ProjectedInput::Message(message)
            if message.role == Role::User && message.content == "recent question"
    ));
}

#[test]
fn incompatible_route_and_neutral_projection_keep_the_original_history() {
    let inputs = project_inputs_for_route(
        &history(),
        "anthropic",
        "claude-sonnet-5",
        ProviderProtocol::AnthropicMessages,
    )
    .unwrap();
    assert_eq!(inputs.len(), 3);
    assert!(matches!(
        &inputs[0],
        ProjectedInput::Message(message) if message.content == "old question"
    ));
    assert!(matches!(
        &inputs[1],
        ProjectedInput::Message(message) if message.content == "old answer"
    ));
    assert!(matches!(
        &inputs[2],
        ProjectedInput::Message(message) if message.content == "recent question"
    ));

    let neutral = derive_messages(&history());
    assert_eq!(neutral.len(), 3);
    assert_eq!(neutral[0].content, "old question");
    assert_eq!(neutral[2].content, "recent question");
}

#[test]
fn a_checkpoint_cannot_claim_its_own_or_future_sequence() {
    let mut events = history();
    let SessionEventKind::NativeCompactionApplied {
        replaced_upto_seq, ..
    } = &mut events[2].kind
    else {
        panic!("fixture marker missing")
    };
    *replaced_upto_seq = 2;
    assert!(matches!(
        project_inputs_for_route(
            &events,
            "openai",
            "gpt-5.6",
            ProviderProtocol::OpenAiResponses,
        ),
        Err(heycode_session::ProjectionError::InvalidCompaction { seq: 2 })
    ));
}

#[test]
fn switch_barrier_names_the_pre_checkpoint_fork_and_later_portable_wins_a_tie() {
    let mut events = history();
    let barrier = opaque_compaction_barrier(&events, "anthropic", "claude-sonnet-5")
        .unwrap()
        .expect("cross-route native checkpoint requires a decision");
    assert_eq!(barrier.provider(), "openai");
    assert_eq!(barrier.model(), "gpt-5.6");
    assert_eq!(barrier.replaced_upto_seq(), 1);
    assert_eq!(barrier.settlement_seq(), 2);
    assert_eq!(barrier.fork_event_count(), 2);
    assert!(
        opaque_compaction_barrier(&events, "openai", "gpt-5.6")
            .unwrap()
            .is_none()
    );

    events.push(event(
        4,
        SessionEventKind::CompactionApplied {
            summary: "portable replacement".to_owned(),
            replaced_upto_seq: 1,
        },
    ));
    assert!(
        opaque_compaction_barrier(&events, "anthropic", "claude-sonnet-5")
            .unwrap()
            .is_none(),
        "the later portable settlement must supersede an equal native boundary"
    );
}

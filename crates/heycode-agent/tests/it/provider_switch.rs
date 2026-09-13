//! C14 opaque-state portable/fork/cancel execution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{AutoApprove, OpaqueStateResolution, ProviderSwitchPreparation};
use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_llm::testing::FakeProvider;
use heycode_session::{Session, SessionEventKind, TurnEndReason};
use tokio_util::sync::CancellationToken;

use super::turn::{World, build_provider_in, script_text};

fn world() -> World {
    build_provider_in(
        tempfile::tempdir().unwrap(),
        |_| Arc::new(FakeProvider::new(vec![script_text("portable summary")])),
        Arc::new(AutoApprove),
    )
}

fn checkpoint() -> ProviderStateItem {
    ProviderStateItem::new(
        "fake",
        "test-model",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant",
            "content":"opaque checkpoint",
            "reasoning_content":"retained state"
        }),
    )
    .unwrap()
}

fn seed_native(world: &World) {
    let mut session = world.session.lock().unwrap();
    for (turn, user, assistant) in [
        (0, "old question", "old answer"),
        (1, "recent question", "recent answer"),
    ] {
        session
            .append(SessionEventKind::UserMessage {
                text: user.to_owned(),
            })
            .unwrap();
        session
            .append(SessionEventKind::TurnStart { turn })
            .unwrap();
        session
            .append(SessionEventKind::AssistantMessage {
                turn,
                step: 0,
                content: assistant.to_owned(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            })
            .unwrap();
        session
            .append(SessionEventKind::TurnEnd {
                turn,
                reason: TurnEndReason::Stop,
            })
            .unwrap();
    }
    session
        .append(SessionEventKind::NativeCompactionApplied {
            strategy: "provider-native".to_owned(),
            replaced_upto_seq: 3,
            items: vec![checkpoint()],
            usage: None,
        })
        .unwrap();
}

#[tokio::test]
async fn portable_fork_and_cancel_are_distinct_and_commit_only_the_selected_effect() {
    let world = world();
    seed_native(&world);
    let barrier = world
        .agent
        .provider_switch_barrier("other", "other-model")
        .unwrap()
        .expect("native state requires a resolution");
    assert_eq!(barrier.fork_event_count(), 8);
    let before = world.session.lock().unwrap().events().len();

    assert!(matches!(
        world
            .agent
            .prepare_provider_switch(
                "other",
                "other-model",
                OpaqueStateResolution::Cancel,
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        ProviderSwitchPreparation::Cancelled
    ));
    assert_eq!(world.session.lock().unwrap().events().len(), before);

    let forked = world
        .agent
        .prepare_provider_switch(
            "other",
            "other-model",
            OpaqueStateResolution::ForkBeforeCheckpoint,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let ProviderSwitchPreparation::Forked {
        session_dir,
        fork_event_count,
        ..
    } = forked
    else {
        panic!("expected a durable fork")
    };
    assert_eq!(fork_event_count, 8);
    assert_eq!(world.session.lock().unwrap().events().len(), before);
    let child = Session::open(session_dir).unwrap();
    assert!(
        heycode_session::opaque_compaction_barrier(child.events(), "other", "other-model")
            .unwrap()
            .is_none(),
        "the child ends immediately before native settlement"
    );

    assert!(matches!(
        world
            .agent
            .prepare_provider_switch(
                "other",
                "other-model",
                OpaqueStateResolution::PortableRecompact,
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        ProviderSwitchPreparation::Ready
    ));
    assert!(
        world
            .agent
            .provider_switch_barrier("other", "other-model")
            .unwrap()
            .is_none()
    );
    let session = world.session.lock().unwrap();
    assert_eq!(session.events().len(), before + 1);
    assert!(matches!(
        session.events().last().map(|event| &event.kind),
        Some(SessionEventKind::CompactionApplied { summary, replaced_upto_seq })
            if summary == "portable summary" && *replaced_upto_seq == 3
    ));
}

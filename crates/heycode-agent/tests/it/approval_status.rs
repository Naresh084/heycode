//! U05 effective approval-policy identity.

use heycode_agent::{
    ApprovalPolicy, ApprovalPolicyKind, AutoApprove, DenyAll, InteractiveApproval,
};

#[test]
fn built_in_policies_report_their_effective_kind() {
    assert_eq!(AutoApprove.kind(), ApprovalPolicyKind::FullAccess);
    assert_eq!(DenyAll.kind(), ApprovalPolicyKind::Deny);
    assert_eq!(
        InteractiveApproval::new(heycode_core::EventBus::default()).kind(),
        ApprovalPolicyKind::Ask
    );
}

/// A turn parked on an unanswered approval dialog must end when the turn is
/// cancelled. Before the batch owned an admission token, `Agent::admit_call`
/// awaited `decide` — uncancellable — so nothing but a human answer could
/// release the turn: the lease stayed held, later inbox submissions were told
/// `Queued`, and `AgentIdle` never fired.
#[tokio::test]
async fn cancelling_a_turn_withdraws_an_unanswered_approval_ask() {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::sync::Arc;

    let policy = Arc::new(InteractiveApproval::new(heycode_core::EventBus::default()));
    let world = super::turn::build(
        vec![
            vec![
                heycode_llm::StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("r1".to_owned()),
                    name: Some("read".to_owned()),
                    arguments_delta: serde_json::json!({"path": "Cargo.toml"}).to_string(),
                },
                heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            vec![
                heycode_llm::StreamChunk::TextDelta("done".to_owned()),
                heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::Stop),
            ],
        ],
        policy.clone() as Arc<dyn ApprovalPolicy>,
    );

    let caller = tokio_util::sync::CancellationToken::new();
    let agent = world.agent.clone();
    let turn = tokio::spawn({
        let caller = caller.clone();
        async move { agent.send_cancellable("read it", caller).await }
    });

    // Wait for the dialog nobody is going to answer.
    for _ in 0..600 {
        if policy.pending_id().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        policy.pending_id().is_some(),
        "the interactive policy never parked on an ask"
    );

    caller.cancel();

    let report = tokio::time::timeout(std::time::Duration::from_secs(20), turn)
        .await
        .expect("a cancelled turn must not stay parked on an unanswered ask")
        .unwrap();
    report.expect("cancellation ends the turn rather than failing it");
    assert_eq!(
        policy.pending_id(),
        None,
        "the withdrawn ask must leave no waiter behind"
    );
    assert!(
        !world.agent.token().is_turn_active(),
        "the turn lease must be released once the ask is withdrawn"
    );
}

#[tokio::test]
async fn accepted_edits_remembers_only_explicit_grants_for_identical_tool_and_inputs() {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use heycode_tools::{ToolCallInput, Verdict};
    use std::sync::Arc;
    let interactive = Arc::new(InteractiveApproval::new(heycode_core::EventBus::default()));
    let policy = Arc::new(heycode_agent::AcceptedEdits::new(interactive.clone()));
    let call = ToolCallInput {
        name: "mcp__files__edit".into(),
        args: serde_json::json!({"path":"a.txt", "old":"a", "new":"b"}),
    };
    // A one-time Accept grants no future calls, even identical ones.
    for answer in [
        heycode_agent::AskAnswer::Allow,
        heycode_agent::AskAnswer::AllowSession,
    ] {
        let task = tokio::spawn({
            let policy = policy.clone();
            let call = call.clone();
            async move { policy.decide(&call).await }
        });
        interactive.answer_with(wait_for_permission(&interactive).await, answer);
        assert!(matches!(task.await.unwrap(), Verdict::Allow));
    }
    let reordered = ToolCallInput {
        name: "mcp__files__edit".into(),
        args: serde_json::from_str(r#"{"new":"b","old":"a","path":"a.txt"}"#).unwrap(),
    };
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), policy.decide(&reordered))
            .await
            .unwrap(),
        Verdict::Allow
    ));
    for changed in [
        ToolCallInput {
            name: "mcp__files__edit".into(),
            args: serde_json::json!({"path":"b.txt", "old":"a", "new":"b"}),
        },
        ToolCallInput {
            name: "mcp__files__edit".into(),
            args: serde_json::json!({"path":"a.txt", "old":"a", "new":"c"}),
        },
        ToolCallInput {
            name: "mcp__files__write".into(),
            args: call.args.clone(),
        },
    ] {
        let task = tokio::spawn({
            let policy = policy.clone();
            async move { policy.decide(&changed).await }
        });
        interactive.answer(wait_for_permission(&interactive).await, false);
        assert!(matches!(task.await.unwrap(), Verdict::Deny { .. }));
    }
    // Concurrent identical first calls share only an explicit future grant.
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let policy = policy.clone();
        tasks.push(tokio::spawn(async move {
            policy
                .decide(&ToolCallInput {
                    name: "bash".into(),
                    args: serde_json::json!({"command":"cargo test"}),
                })
                .await
        }));
    }
    interactive.answer_with(
        wait_for_permission(&interactive).await,
        heycode_agent::AskAnswer::AllowSession,
    );
    for task in tasks {
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            Verdict::Allow
        ));
    }
    assert!(interactive.pending_id().is_none());
}

#[tokio::test]
async fn cancelled_and_denied_requests_never_grant_accepted_edits_permission() {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use heycode_tools::{ToolCallInput, Verdict};
    use std::sync::Arc;
    let interactive = Arc::new(InteractiveApproval::new(heycode_core::EventBus::default()));
    let policy = Arc::new(heycode_agent::AcceptedEdits::new(interactive.clone()));
    for outcome in [None, Some(false), Some(true)] {
        let token = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn({
            let policy = policy.clone();
            let token = token.clone();
            async move {
                policy
                    .decide_cancellable(
                        &ToolCallInput {
                            name: "mcp__files__write".into(),
                            args: serde_json::json!({"path":"a"}),
                        },
                        token,
                    )
                    .await
            }
        });
        let id = wait_for_permission(&interactive).await;
        if let Some(allow) = outcome {
            interactive.answer(id, allow);
        } else {
            token.cancel();
        }
        let verdict = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(matches!(verdict, Verdict::Allow), outcome == Some(true));
        assert!(interactive.pending_id().is_none());
    }
}

#[tokio::test]
async fn default_always_asks_despite_session_grants_and_mode_changes_do_not_leak_approval() {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use heycode_tools::{ToolCallInput, Verdict};
    use std::sync::Arc;
    let interactive = Arc::new(InteractiveApproval::new(heycode_core::EventBus::default()));
    let policy = Arc::new(heycode_agent::SwitchableApproval::new(
        interactive.clone(),
        Some(interactive.clone()),
    ));
    let call = ToolCallInput {
        name: "mcp__files__read".into(),
        args: serde_json::json!({"path":"a"}),
    };
    policy.switch(ApprovalPolicyKind::AcceptedEdits).unwrap();
    let task = tokio::spawn({
        let policy = policy.clone();
        let call = call.clone();
        async move { policy.decide(&call).await }
    });
    interactive.answer_with(
        wait_for_permission(&interactive).await,
        heycode_agent::AskAnswer::AllowSession,
    );
    assert!(matches!(task.await.unwrap(), Verdict::Allow));
    policy.switch(ApprovalPolicyKind::Ask).unwrap();
    for _ in 0..2 {
        let task = tokio::spawn({
            let policy = policy.clone();
            let call = call.clone();
            async move { policy.decide(&call).await }
        });
        interactive.answer_with(
            wait_for_permission(&interactive).await,
            heycode_agent::AskAnswer::AllowSession,
        );
        assert!(matches!(task.await.unwrap(), Verdict::Allow));
    }
    assert!(interactive.session_rules().is_empty());
    assert!(policy.switch(ApprovalPolicyKind::Auto).is_err());
    assert_eq!(policy.kind(), ApprovalPolicyKind::Ask);
    policy.switch(ApprovalPolicyKind::AcceptedEdits).unwrap();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), policy.decide(&call))
            .await
            .unwrap(),
        Verdict::Allow
    ));
    // A new conversation owns an empty grant set.
    let fresh = Arc::new(heycode_agent::AcceptedEdits::new(interactive.clone()));
    let task = tokio::spawn(async move { fresh.decide(&call).await });
    interactive.answer(wait_for_permission(&interactive).await, false);
    assert!(matches!(task.await.unwrap(), Verdict::Deny { .. }));
}

async fn wait_for_permission(policy: &InteractiveApproval) -> u64 {
    #![allow(clippy::expect_used)]
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(id) = policy.pending_id() {
                break id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("permission prompt must become visible")
}

#[tokio::test]
async fn accepted_edits_allows_different_file_changes_but_default_and_commands_still_ask() {
    #![allow(clippy::unwrap_used)]
    use heycode_tools::{ToolCallInput, Verdict};
    use std::sync::Arc;
    let interactive = Arc::new(InteractiveApproval::new(heycode_core::EventBus::default()));
    let policy =
        heycode_agent::SwitchableApproval::new(interactive.clone(), Some(interactive.clone()));
    policy
        .switch_by_user(ApprovalPolicyKind::AcceptedEdits)
        .unwrap();
    for name in ["read", "glob", "grep", "edit", "write", "apply_patch"] {
        for path in ["first.txt", "second.txt"] {
            let verdict = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                policy.decide(&ToolCallInput {
                    name: name.into(),
                    args: serde_json::json!({"path": path, "new": "different content"}),
                }),
            )
            .await
            .unwrap();
            assert!(matches!(verdict, Verdict::Allow));
            assert!(interactive.pending_id().is_none());
        }
    }
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        policy
            .decide_cancellable(
                &ToolCallInput {
                    name: "write".into(),
                    args: serde_json::json!({"path":"a"})
                },
                cancelled
            )
            .await,
        Verdict::Deny { .. }
    ));
    let policy = Arc::new(policy);
    for name in ["bash", "mcp__files__write", "web_fetch"] {
        let pending = tokio::spawn({
            let policy = policy.clone();
            async move {
                policy
                    .decide(&ToolCallInput {
                        name: name.into(),
                        args: serde_json::json!({}),
                    })
                    .await
            }
        });
        interactive.answer(wait_for_permission(&interactive).await, false);
        assert!(matches!(pending.await.unwrap(), Verdict::Deny { .. }));
    }
    policy.switch_by_user(ApprovalPolicyKind::Ask).unwrap();
    let pending = tokio::spawn({
        let policy = policy.clone();
        async move {
            policy
                .decide(&ToolCallInput {
                    name: "write".into(),
                    args: serde_json::json!({"path":"third.txt"}),
                })
                .await
        }
    });
    interactive.answer(wait_for_permission(&interactive).await, false);
    assert!(matches!(pending.await.unwrap(), Verdict::Deny { .. }));
}

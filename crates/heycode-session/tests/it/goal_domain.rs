//! O10 durable goal snapshots and source-attributed admitted rounds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    GoalChange, GoalId, GoalOperation, GoalPhase, GoalSnapshot, InboxDelivery, InboxMessage,
    InboxMessageId, InboxSource, InboxTarget, Session, SessionEventKind, project_goal,
};

fn snapshot(id: &GoalId, revision: u64, phase: GoalPhase) -> GoalSnapshot {
    GoalSnapshot::new(
        id.clone(),
        revision,
        "finish the durable orchestration lane",
        phase,
        None,
        3,
    )
    .unwrap()
}

#[test]
fn full_snapshots_cas_and_admitted_rounds_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let id = GoalId::new("goal-1").unwrap();
    session
        .append(SessionEventKind::GoalChange {
            change: Box::new(GoalChange::snapshot(
                GoalOperation::Create,
                snapshot(&id, 1, GoalPhase::Active),
                0,
                100,
                100,
            )),
        })
        .unwrap();

    let round = InboxMessage::with_source(
        InboxMessageId::new("goal-1-round-1").unwrap(),
        InboxDelivery::FollowUp,
        "<goal_round>round one</goal_round>",
        InboxSource::Goal {
            goal_id: id.clone(),
            revision: 1,
            round: 1,
        },
    )
    .unwrap();
    session
        .append(SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![round],
            outcome: None,
        })
        .unwrap();
    session
        .append_inbox_claim(InboxTarget::NextTurn, 0)
        .unwrap();

    session
        .append(SessionEventKind::GoalChange {
            change: Box::new(GoalChange::snapshot(
                GoalOperation::Pause,
                snapshot(&id, 2, GoalPhase::Paused),
                1,
                100,
                120,
            )),
        })
        .unwrap();
    session.flush().unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);

    let reopened = Session::open(directory).unwrap();
    let goal = project_goal(reopened.events()).unwrap();
    let current = goal.current().unwrap();
    assert_eq!(current.snapshot().id(), &id);
    assert_eq!(current.snapshot().revision(), 2);
    assert_eq!(current.snapshot().phase(), GoalPhase::Paused);
    assert_eq!(current.rounds_started(), 1);
    assert_eq!(current.created_at_ms(), 100);
    assert_eq!(current.updated_at_ms(), 120);
}

#[test]
fn stale_revision_illegal_transition_and_skipped_round_fail_loud() {
    let id = GoalId::new("goal-invalid").unwrap();
    let created = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 0,
        time_ms: 1,
        kind: SessionEventKind::GoalChange {
            change: Box::new(GoalChange::snapshot(
                GoalOperation::Create,
                snapshot(&id, 1, GoalPhase::Active),
                0,
                1,
                1,
            )),
        },
    };
    let stale = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 1,
        time_ms: 2,
        kind: SessionEventKind::GoalChange {
            change: Box::new(GoalChange::snapshot(
                GoalOperation::Pause,
                snapshot(&id, 3, GoalPhase::Paused),
                0,
                1,
                2,
            )),
        },
    };
    assert!(project_goal(&[created.clone(), stale]).is_err());

    let completed = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 1,
        time_ms: 2,
        kind: SessionEventKind::GoalChange {
            change: Box::new(GoalChange::snapshot(
                GoalOperation::Complete,
                snapshot(&id, 2, GoalPhase::Complete),
                0,
                1,
                2,
            )),
        },
    };
    let resumed = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 2,
        time_ms: 3,
        kind: SessionEventKind::GoalChange {
            change: Box::new(GoalChange::snapshot(
                GoalOperation::Resume,
                snapshot(&id, 3, GoalPhase::Active),
                0,
                1,
                3,
            )),
        },
    };
    assert!(project_goal(&[created.clone(), completed, resumed]).is_err());

    let skipped = InboxMessage::with_source(
        InboxMessageId::new("goal-invalid-round-2").unwrap(),
        InboxDelivery::FollowUp,
        "round two",
        InboxSource::Goal {
            goal_id: id,
            revision: 1,
            round: 2,
        },
    )
    .unwrap();
    let inserted = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 1,
        time_ms: 2,
        kind: SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![skipped],
            outcome: None,
        },
    };
    let claimed = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 2,
        time_ms: 3,
        kind: SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(1),
            inserted: Vec::new(),
            outcome: None,
        },
    };
    let admitted = heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: 3,
        time_ms: 3,
        kind: SessionEventKind::UserMessage {
            text: "round two".to_owned(),
        },
    };
    assert!(project_goal(&[created, inserted, claimed, admitted]).is_err());
}

#[test]
fn v1_cannot_claim_goal_change_or_source_attribution() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("v1-goal");
    std::fs::create_dir_all(&directory).unwrap();
    let raw = serde_json::json!({
        "v": 1,
        "seq": 0,
        "time_ms": 1,
        "kind": "goal/change",
        "data": {}
    });
    std::fs::write(directory.join("session.jsonl"), format!("{raw}\n")).unwrap();
    assert!(matches!(
        Session::open(directory),
        Err(heycode_session::OpenError::UnknownKind { .. })
    ));
}

#[test]
fn goal_change_wire_uses_one_versioned_operation_not_an_internal_wrapper_tag() {
    let id = GoalId::new("goal-wire").unwrap();
    let change = GoalChange::snapshot(
        GoalOperation::Create,
        snapshot(&id, 1, GoalPhase::Active),
        0,
        10,
        10,
    );
    let value = serde_json::to_value(change).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["operation"], "create");
    assert!(value.get("action").is_none());
}

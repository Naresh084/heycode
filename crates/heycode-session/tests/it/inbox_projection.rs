//! Durable inbox splice projection and resume contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    AppendError, InboxDelivery, InboxMessage, InboxMessageId, InboxSpliceOutcome, InboxTarget,
    OpenError, Session, SessionEvent, SessionEventKind, derive_messages, project_inbox,
};

fn message(id: &str, delivery: InboxDelivery, text: &str) -> InboxMessage {
    InboxMessage::with_id(InboxMessageId::new(id).unwrap(), delivery, text).unwrap()
}

fn append_splice(
    session: &mut Session,
    target: InboxTarget,
    start: u32,
    removed_count: Option<u32>,
    inserted: Vec<InboxMessage>,
    outcome: Option<InboxSpliceOutcome>,
) {
    session
        .append(SessionEventKind::AgentInboxSplice {
            target,
            start,
            removed_count,
            inserted,
            outcome,
        })
        .unwrap();
}

#[test]
fn follow_up_steer_and_inject_pending_state_survives_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let session_dir = session.path().parent().unwrap().to_path_buf();
    let follow_up = message("follow-1", InboxDelivery::FollowUp, "do this next");
    let injected = message("inject-1", InboxDelivery::Inject, "build finished");
    let steering = message("steer-1", InboxDelivery::Steer, "change direction");

    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![follow_up.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        0,
        None,
        vec![injected.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        1,
        None,
        vec![steering.clone()],
        None,
    );

    assert_eq!(
        session.inbox().next_turn(),
        std::slice::from_ref(&follow_up)
    );
    assert_eq!(
        session.inbox().next_step(),
        &[injected.clone(), steering.clone()]
    );
    drop(session);

    let resumed = Session::open(session_dir).unwrap();
    assert_eq!(resumed.inbox().next_turn(), &[follow_up]);
    assert_eq!(resumed.inbox().next_step(), &[injected, steering]);
    assert!(resumed.inbox().claimed().is_empty());
    assert!(resumed.inbox().canceled().is_empty());
    assert_eq!(project_inbox(resumed.events()).unwrap(), *resumed.inbox());
}

#[test]
fn splice_wire_schema_is_exact_and_omits_zero_or_unsettled_fields() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        0,
        None,
        vec![message("inject-1", InboxDelivery::Inject, "context")],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        0,
        Some(1),
        Vec::new(),
        Some(InboxSpliceOutcome::Canceled),
    );

    let lines = std::fs::read_to_string(session.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines[0]["kind"], "agent/inbox/splice");
    assert_eq!(
        lines[0]["data"],
        serde_json::json!({
            "target":"next_step",
            "start":0,
            "inserted":[{"id":"inject-1","delivery":"inject","text":"context"}]
        })
    );
    assert_eq!(
        lines[1]["data"],
        serde_json::json!({
            "target":"next_step",
            "start":0,
            "removed_count":1,
            "inserted":[],
            "outcome":"canceled"
        })
    );
}

#[test]
fn claims_and_cancellations_remain_accounted_after_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let session_dir = session.path().parent().unwrap().to_path_buf();
    let follow_up = message("follow-1", InboxDelivery::FollowUp, "later");
    let injected = message("inject-1", InboxDelivery::Inject, "context");
    let steering = message("steer-1", InboxDelivery::Steer, "now");

    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![follow_up.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        0,
        None,
        vec![injected.clone(), steering.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        0,
        Some(2),
        Vec::new(),
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        Some(1),
        Vec::new(),
        Some(InboxSpliceOutcome::Canceled),
    );
    drop(session);

    let resumed = Session::open(session_dir).unwrap();
    assert!(resumed.inbox().next_turn().is_empty());
    assert!(resumed.inbox().next_step().is_empty());
    assert_eq!(
        resumed
            .inbox()
            .claimed()
            .iter()
            .map(|settlement| settlement.message().id().as_str())
            .collect::<Vec<_>>(),
        vec!["inject-1", "steer-1"]
    );
    assert_eq!(
        resumed
            .inbox()
            .canceled()
            .iter()
            .map(|settlement| settlement.message().id().as_str())
            .collect::<Vec<_>>(),
        vec!["follow-1"]
    );
}

#[test]
fn replacement_settles_old_message_and_preserves_normalized_order() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let first = message("follow-1", InboxDelivery::FollowUp, "first");
    let old = message("follow-old", InboxDelivery::FollowUp, "old");
    let replacement = message("follow-new", InboxDelivery::FollowUp, "new");

    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![first.clone(), old.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        1,
        Some(1),
        vec![replacement.clone()],
        Some(InboxSpliceOutcome::Canceled),
    );

    assert_eq!(session.inbox().next_turn(), &[first, replacement]);
    assert_eq!(session.inbox().canceled().len(), 1);
    assert_eq!(session.inbox().canceled()[0].message(), &old);
}

#[test]
fn invalid_splice_fails_before_append_and_does_not_publish_or_write() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let before = std::fs::read(session.path()).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    session
        .bus()
        .on::<SessionEvent>(move |event| sink.lock().unwrap().push(event.seq));
    let error = session
        .append(SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 1,
            removed_count: None,
            inserted: vec![message(
                "follow-1",
                InboxDelivery::FollowUp,
                "out of bounds",
            )],
            outcome: None,
        })
        .unwrap_err();

    assert!(matches!(error, AppendError::InvalidEvent { .. }));
    assert!(session.events().is_empty());
    assert!(session.inbox().next_turn().is_empty());
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(session.path()).unwrap(), before);
}

#[test]
fn non_normalized_shapes_and_delivery_target_mismatch_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let cases = vec![
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: Vec::new(),
            outcome: None,
        },
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(0),
            inserted: Vec::new(),
            outcome: None,
        },
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![message("follow-1", InboxDelivery::FollowUp, "later")],
            outcome: Some(InboxSpliceOutcome::Canceled),
        },
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(1),
            inserted: vec![message("follow-2", InboxDelivery::FollowUp, "replace")],
            outcome: None,
        },
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextStep,
            start: 0,
            removed_count: None,
            inserted: vec![message("follow-3", InboxDelivery::FollowUp, "wrong list")],
            outcome: None,
        },
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                message("duplicate", InboxDelivery::FollowUp, "one"),
                message("duplicate", InboxDelivery::FollowUp, "two"),
            ],
            outcome: None,
        },
    ];

    for kind in cases {
        assert!(matches!(
            session.append(kind),
            Err(AppendError::InvalidEvent { .. })
        ));
    }
    assert!(session.events().is_empty());
    assert!(session.inbox().next_turn().is_empty());
    assert!(session.inbox().next_step().is_empty());
}

#[test]
fn duplicate_pending_identity_across_lists_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![message("same", InboxDelivery::FollowUp, "one")],
        None,
    );
    let event_count = session.events().len();

    let error = session
        .append(SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextStep,
            start: 0,
            removed_count: None,
            inserted: vec![message("same", InboxDelivery::Steer, "two")],
            outcome: None,
        })
        .unwrap_err();

    assert!(matches!(error, AppendError::InvalidEvent { .. }));
    assert_eq!(session.events().len(), event_count);
}

#[test]
fn occurrence_identity_cannot_be_reused_after_claim_or_in_a_replacement() {
    let root = tempfile::tempdir().unwrap();
    let mut claimed_session = Session::create(root.path()).unwrap();
    append_splice(
        &mut claimed_session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![message("once", InboxDelivery::FollowUp, "first")],
        None,
    );
    append_splice(
        &mut claimed_session,
        InboxTarget::NextTurn,
        0,
        Some(1),
        Vec::new(),
        None,
    );
    let claimed_session_dir = claimed_session.path().parent().unwrap().to_path_buf();
    drop(claimed_session);
    let mut claimed_session = Session::open(claimed_session_dir).unwrap();
    let claimed_event_count = claimed_session.events().len();
    assert!(matches!(
        claimed_session.append(SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![message("once", InboxDelivery::FollowUp, "second")],
            outcome: None,
        }),
        Err(AppendError::InvalidEvent { .. })
    ));
    assert_eq!(claimed_session.events().len(), claimed_event_count);
    assert_eq!(claimed_session.inbox().claimed().len(), 1);
    assert!(claimed_session.inbox().next_turn().is_empty());

    let other_root = tempfile::tempdir().unwrap();
    let mut replacement_session = Session::create(other_root.path()).unwrap();
    let original = message("stable", InboxDelivery::FollowUp, "original");
    append_splice(
        &mut replacement_session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![original.clone()],
        None,
    );
    let replacement_event_count = replacement_session.events().len();
    assert!(matches!(
        replacement_session.append(SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(1),
            inserted: vec![message("stable", InboxDelivery::FollowUp, "edited")],
            outcome: Some(InboxSpliceOutcome::Canceled),
        }),
        Err(AppendError::InvalidEvent { .. })
    ));
    assert_eq!(replacement_session.events().len(), replacement_event_count);
    assert_eq!(
        replacement_session.inbox().next_turn(),
        std::slice::from_ref(&original)
    );
    assert!(replacement_session.inbox().canceled().is_empty());
}

#[test]
fn persisted_out_of_bounds_splice_fails_resume_loudly() {
    let root = tempfile::tempdir().unwrap();
    let session_dir = root.path().join("bad-inbox");
    std::fs::create_dir_all(&session_dir).unwrap();
    let line = serde_json::json!({
        "v": 2,
        "seq": 0,
        "time_ms": 1,
        "kind": "agent/inbox/splice",
        "data": {
            "target": "next_turn",
            "start": 4,
            "inserted": [{"id":"follow-1","delivery":"follow_up","text":"later"}]
        }
    });
    std::fs::write(session_dir.join("session.jsonl"), format!("{line}\n")).unwrap();

    assert!(matches!(
        Session::open(session_dir),
        Err(OpenError::InvalidEvent { line_no: 1, .. })
    ));
}

#[test]
fn deserialized_message_identity_and_text_are_revalidated() {
    let root = tempfile::tempdir().unwrap();
    for (name, id, text) in [
        ("bad-id", "bad id", "valid"),
        ("unsafe-id", "bidi-\u{202e}", "valid"),
        ("bad-text", "valid-id", "   "),
    ] {
        let session_dir = root.path().join(name);
        std::fs::create_dir_all(&session_dir).unwrap();
        let line = serde_json::json!({
            "v": 2,
            "seq": 0,
            "time_ms": 1,
            "kind": "agent/inbox/splice",
            "data": {
                "target": "next_turn",
                "start": 0,
                "inserted": [{"id":id,"delivery":"follow_up","text":text}]
            }
        });
        std::fs::write(session_dir.join("session.jsonl"), format!("{line}\n")).unwrap();
        // `InvalidEvent`, not `CorruptLine`: the query path retries a corrupt
        // line as a possibly-transient partial write, and an invalid id is
        // permanent. The classification is the contract, not just the refusal.
        let error = Session::open(session_dir).err();
        assert!(
            matches!(error, Some(OpenError::InvalidEvent { line_no: 1, .. })),
            "for {name}: {error:?}"
        );
    }
}

#[test]
fn compaction_does_not_shadow_operational_pending_inbox_state() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let pending = message("follow-1", InboxDelivery::FollowUp, "still pending");
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![pending.clone()],
        None,
    );
    session
        .append(SessionEventKind::CompactionApplied {
            summary: "old history".to_owned(),
            replaced_upto_seq: 0,
        })
        .unwrap();

    assert_eq!(
        project_inbox(session.events()).unwrap().next_turn(),
        &[pending]
    );
}

#[test]
fn queued_and_claimed_splices_are_not_model_visible_until_admission_is_logged() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let pending = message("follow-1", InboxDelivery::FollowUp, "exact input");
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![pending],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        Some(1),
        Vec::new(),
        None,
    );
    assert!(derive_messages(session.events()).is_empty());

    session
        .append(SessionEventKind::UserMessage {
            text: "exact input".to_owned(),
        })
        .unwrap();
    let messages = derive_messages(session.events());
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "exact input");
}

#[test]
fn human_recall_preserves_order_excludes_consumed_and_operational_messages_and_survives_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let first = message("first", InboxDelivery::Steer, "same text");
    let second = message("second", InboxDelivery::FollowUp, "second\nline");
    let third = message("third", InboxDelivery::Steer, "same text");
    let job = InboxMessage::with_source(
        InboxMessageId::new("job-msg").unwrap(),
        InboxDelivery::FollowUp,
        "job output",
        heycode_session::InboxSource::Job {
            job_id: "job-1".into(),
        },
    )
    .unwrap();
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        0,
        None,
        vec![first.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextTurn,
        0,
        None,
        vec![job.clone(), second.clone()],
        None,
    );
    append_splice(
        &mut session,
        InboxTarget::NextStep,
        1,
        None,
        vec![third.clone()],
        None,
    );
    assert_eq!(
        session.pending_human_messages(),
        vec![first, second.clone(), third.clone()]
    );
    session
        .append_inbox_claim(InboxTarget::NextStep, 0)
        .unwrap();
    assert_eq!(
        session.recall_human_messages().unwrap(),
        vec![second, third]
    );
    assert_eq!(session.inbox().next_turn(), &[job]);
    assert!(session.inbox().next_step().is_empty());
    assert!(session.recall_human_messages().unwrap().is_empty());
    let directory = session.path().parent().unwrap().to_owned();
    drop(session);
    let resumed = Session::open(directory).unwrap();
    assert!(resumed.pending_human_messages().is_empty());
    assert_eq!(resumed.inbox().claimed().len(), 1);
    assert_eq!(resumed.inbox().canceled().len(), 2);
    assert_eq!(resumed.events().iter().filter(|event| matches!(&event.kind, SessionEventKind::UserMessage { text } if text == "same text")).count(), 1);
}

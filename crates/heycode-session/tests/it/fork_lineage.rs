//! Shared-prefix fork and durable lineage contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    ForkBoundary, ForkError, OpenError, Session, SessionCreationMetadata, SessionEventKind,
    SessionSource, TurnEndReason, derive_messages,
};

fn metadata(cwd: &str, runtime: &str, source: SessionSource) -> SessionCreationMetadata {
    SessionCreationMetadata::new(
        Some(std::path::PathBuf::from(cwd)),
        Some(runtime.to_owned()),
        source,
    )
    .unwrap()
}

fn append_closed_turn(session: &mut Session, turn: u64, text: &str) {
    session
        .append(SessionEventKind::TurnStart { turn })
        .unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: text.to_owned(),
        })
        .unwrap();
    session
        .append(SessionEventKind::TurnEnd {
            turn,
            reason: TurnEndReason::Stop,
        })
        .unwrap();
}

#[test]
fn latest_fork_reuses_verified_prefix_without_copying_or_mutating_parent_bytes() {
    let root = tempfile::tempdir().unwrap();
    let mut parent = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    append_closed_turn(&mut parent, 1, "parent-only prompt");
    parent
        .append(SessionEventKind::SessionTitle {
            title: "Parent title".to_owned(),
        })
        .unwrap();
    let parent_id = parent.id().clone();
    let parent_path = parent.path().to_path_buf();
    let parent_events = parent.events().to_vec();
    let parent_bytes = std::fs::read(&parent_path).unwrap();

    let mut child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();
    let child_path = child.path().to_path_buf();
    assert_ne!(child_id, parent_id);
    assert_eq!(
        &child.events()[..parent_events.len()],
        parent_events.as_slice()
    );
    assert_eq!(child.events().len(), parent_events.len() + 1);
    assert_eq!(child.first_local_seq(), parent_events.len() as u64);
    let lineage = child.lineage().unwrap().clone();
    assert_eq!(lineage.parent_session_id(), &parent_id);
    assert_eq!(lineage.seed_event_count(), parent_events.len() as u64);
    assert_eq!(
        child.metadata().unwrap().cwd().unwrap(),
        std::path::Path::new("/work/project")
    );
    assert_eq!(child.metadata().unwrap().runtime(), Some("native"));
    assert_eq!(child.metadata().unwrap().source(), SessionSource::Fork);

    let child_physical = std::fs::read_to_string(&child_path).unwrap();
    assert_eq!(child_physical.lines().count(), 1);
    assert!(!child_physical.contains("parent-only prompt"));
    let child_line: serde_json::Value = serde_json::from_str(&child_physical).unwrap();
    assert_eq!(child_line["kind"], "session/created");
    assert_eq!(child_line["seq"], parent_events.len());
    assert_eq!(
        child_line["data"]["creation"]["metadata"],
        serde_json::json!({
            "cwd":"/work/project",
            "runtime":"native",
            "source":"fork"
        })
    );
    assert_eq!(
        child_line["data"]["creation"]["parent"]["parent_session_id"],
        parent_id.as_str()
    );
    assert_eq!(
        child_line["data"]["creation"]["parent"]["seed_event_count"],
        parent_events.len()
    );
    assert_eq!(
        child_line["data"]["creation"]["parent"]["prefix_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(std::fs::read(&parent_path).unwrap(), parent_bytes);

    child
        .append(SessionEventKind::UserMessage {
            text: "child-only prompt".to_owned(),
        })
        .unwrap();
    assert_eq!(std::fs::read(&parent_path).unwrap(), parent_bytes);
    drop(child);
    drop(parent);

    let resumed = Session::open(root.path().join(child_id.as_str())).unwrap();
    assert_eq!(resumed.lineage().unwrap(), &lineage);
    let messages = derive_messages(resumed.events());
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "parent-only prompt");
    assert_eq!(messages[1].content, "child-only prompt");
}

#[test]
fn explicit_closed_boundary_can_exclude_a_later_open_tail() {
    let root = tempfile::tempdir().unwrap();
    let mut parent = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Headless),
    )
    .unwrap();
    append_closed_turn(&mut parent, 1, "closed");
    let closed_boundary = parent.events().last().unwrap().seq;
    parent
        .append(SessionEventKind::TurnStart { turn: 2 })
        .unwrap();
    parent
        .append(SessionEventKind::UserMessage {
            text: "open tail".to_owned(),
        })
        .unwrap();

    assert!(matches!(
        parent.fork(root.path(), ForkBoundary::Latest),
        Err(ForkError::OpenTurn { turn: 2 })
    ));
    assert!(matches!(
        parent.fork(root.path(), ForkBoundary::Through(closed_boundary - 1)),
        Err(ForkError::OpenTurn { turn: 1 })
    ));
    assert!(matches!(
        parent.fork(root.path(), ForkBoundary::Through(9_999)),
        Err(ForkError::InvalidBoundary { .. })
    ));

    let child = parent
        .fork(root.path(), ForkBoundary::Through(closed_boundary))
        .unwrap();
    assert_eq!(
        child.lineage().unwrap().seed_event_count(),
        closed_boundary + 1
    );
    assert!(child.events().iter().all(|event| {
        !matches!(&event.kind, SessionEventKind::UserMessage { text } if text == "open tail")
    }));
}

#[test]
fn parent_growth_after_fork_is_ignored_but_prefix_tampering_fails() {
    let root = tempfile::tempdir().unwrap();
    let mut parent = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    append_closed_turn(&mut parent, 1, "stable prefix");
    let parent_path = parent.path().to_path_buf();
    let child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_dir = child.path().parent().unwrap().to_path_buf();
    drop(child);

    parent
        .append(SessionEventKind::SessionTitle {
            title: "post-fork parent growth".to_owned(),
        })
        .unwrap();
    drop(parent);
    let child_after_growth = Session::open(&child_dir).unwrap();
    assert!(child_after_growth.events().iter().all(|event| {
        !matches!(&event.kind, SessionEventKind::SessionTitle { title } if title == "post-fork parent growth")
    }));
    drop(child_after_growth);

    let mut lines = std::fs::read_to_string(&parent_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let prompt = lines
        .iter_mut()
        .find(|line| line["kind"] == "user/message")
        .unwrap();
    prompt["data"]["text"] = serde_json::json!("tampered prefix");
    let rewritten = lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(parent_path, rewritten).unwrap();
    assert!(matches!(
        Session::open(child_dir),
        Err(OpenError::LineageDigestMismatch { .. })
    ));
}

#[test]
fn nested_forks_stitch_each_shared_prefix_once() {
    let root = tempfile::tempdir().unwrap();
    let mut root_session = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    append_closed_turn(&mut root_session, 1, "root");
    let mut child = root_session
        .fork(root.path(), ForkBoundary::Latest)
        .unwrap();
    append_closed_turn(&mut child, 2, "child");
    let grandchild = child.fork(root.path(), ForkBoundary::Latest).unwrap();
    let grandchild_id = grandchild.id().clone();
    let expected = derive_messages(grandchild.events());
    drop(grandchild);
    drop(child);
    drop(root_session);

    let resumed = Session::open(root.path().join(grandchild_id.as_str())).unwrap();
    assert_eq!(derive_messages(resumed.events()), expected);
    assert_eq!(
        derive_messages(resumed.events())
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["root", "child"]
    );
}

#[test]
fn historical_v1_prefix_forks_without_rewrite_and_child_appends_v2() {
    let root = tempfile::tempdir().unwrap();
    let parent_dir = root.path().join("legacy-parent");
    std::fs::create_dir_all(&parent_dir).unwrap();
    let original = include_str!("../fixtures/session-v1.jsonl");
    let parent_path = parent_dir.join("session.jsonl");
    std::fs::write(&parent_path, original).unwrap();
    let parent = Session::open(&parent_dir).unwrap();
    let expected = derive_messages(parent.events());
    let child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();
    assert!(
        child.events()[..parent.events().len()]
            .iter()
            .all(|event| event.v == 1)
    );
    assert_eq!(child.events().last().unwrap().v, 2);
    assert_eq!(std::fs::read_to_string(&parent_path).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(child.path())
            .unwrap()
            .lines()
            .count(),
        1
    );
    drop(child);
    drop(parent);
    let resumed = Session::open(root.path().join(child_id.as_str())).unwrap();
    assert_eq!(derive_messages(resumed.events()), expected);
}

#[test]
fn creation_metadata_rejects_unsafe_cwd_and_runtime() {
    assert!(
        SessionCreationMetadata::new(
            Some(std::path::PathBuf::from("relative")),
            Some("native".to_owned()),
            SessionSource::Interactive,
        )
        .is_err()
    );
    assert!(
        SessionCreationMetadata::new(
            Some(std::path::PathBuf::from("/work/project")),
            Some("bad runtime".to_owned()),
            SessionSource::Interactive,
        )
        .is_err()
    );
    assert!(
        SessionCreationMetadata::new(
            Some(std::path::PathBuf::from("/work/\u{202e}hidden")),
            Some("native".to_owned()),
            SessionSource::Interactive,
        )
        .is_err()
    );
    assert!(
        SessionCreationMetadata::new(
            Some(std::path::PathBuf::from("/work//project")),
            Some("native".to_owned()),
            SessionSource::Interactive,
        )
        .is_err()
    );
}

#[test]
fn empty_parent_forks_at_zero_and_reopens_without_a_copied_prefix() {
    let root = tempfile::tempdir().unwrap();
    let parent = Session::create(root.path()).unwrap();
    let parent_id = parent.id().clone();
    let child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();
    assert_eq!(child.events().len(), 1);
    assert!(!child.is_fresh());
    assert_eq!(child.events()[0].seq, 0);
    assert_eq!(child.lineage().unwrap().parent_session_id(), &parent_id);
    assert_eq!(child.lineage().unwrap().seed_event_count(), 0);
    assert_eq!(child.first_local_seq(), 0);
    assert_eq!(
        std::fs::read_to_string(child.path())
            .unwrap()
            .lines()
            .count(),
        1
    );
    drop(child);
    drop(parent);
    let resumed = Session::open(root.path().join(child_id.as_str())).unwrap();
    assert_eq!(resumed.lineage().unwrap().seed_event_count(), 0);
}

#[test]
fn zero_event_prefix_can_fork_before_a_first_open_turn() {
    let root = tempfile::tempdir().unwrap();
    let mut parent = Session::create(root.path()).unwrap();
    parent
        .append(SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    parent
        .append(SessionEventKind::UserMessage {
            text: "open first turn".to_owned(),
        })
        .unwrap();

    assert!(matches!(
        parent.fork(root.path(), ForkBoundary::Latest),
        Err(ForkError::OpenTurn { turn: 1 })
    ));
    let child = parent
        .fork(root.path(), ForkBoundary::EventCount(0))
        .unwrap();
    assert_eq!(child.lineage().unwrap().seed_event_count(), 0);
    assert!(child.events().iter().all(|event| {
        !matches!(&event.kind, SessionEventKind::UserMessage { text } if text == "open first turn")
    }));
    assert_eq!(child.first_local_seq(), 0);
}

#[test]
fn creation_event_is_constructor_owned_and_malformed_lineage_never_resolves_a_path() {
    let root = tempfile::tempdir().unwrap();
    let creation = heycode_session::SessionCreation::new(metadata(
        "/work/project",
        "native",
        SessionSource::Interactive,
    ))
    .unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let before = std::fs::read(session.path()).unwrap();
    assert!(
        session
            .append(SessionEventKind::SessionCreated {
                creation: Box::new(creation),
            })
            .is_err()
    );
    assert!(session.events().is_empty());
    assert_eq!(std::fs::read(session.path()).unwrap(), before);
    drop(session);

    let parent = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    let child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_dir = child.path().parent().unwrap().to_path_buf();
    let child_path = child.path().to_path_buf();
    drop(child);
    drop(parent);
    let mut line: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&child_path).unwrap()).unwrap();
    line["data"]["creation"]["parent"]["parent_session_id"] = serde_json::json!("..");
    std::fs::write(&child_path, line.to_string() + "\n").unwrap();
    assert!(matches!(
        Session::open(child_dir),
        Err(OpenError::InvalidEvent { line_no: 1, .. })
    ));
}

#[test]
fn missing_parent_is_a_typed_failure_and_parent_can_be_restored() {
    let root = tempfile::tempdir().unwrap();
    let parent = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    let parent_id = parent.id().clone();
    let parent_dir = parent.path().parent().unwrap().to_path_buf();
    let parked_parent = root.path().join("parked-parent");
    let child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_dir = child.path().parent().unwrap().to_path_buf();
    drop(child);
    drop(parent);

    std::fs::rename(&parent_dir, &parked_parent).unwrap();
    assert!(matches!(
        Session::open(&child_dir),
        Err(OpenError::LineageParentMissing { parent }) if parent == parent_id
    ));
    std::fs::rename(&parked_parent, &parent_dir).unwrap();
    assert!(Session::open(child_dir).is_ok());
}

#[test]
fn lineage_depth_is_bounded_before_an_unopenable_child_is_published() {
    let root = tempfile::tempdir().unwrap();
    let mut current = Session::create(root.path()).unwrap();
    for _ in 0..64 {
        current = current.fork(root.path(), ForkBoundary::Latest).unwrap();
    }
    let deepest = current.path().parent().unwrap().to_path_buf();
    assert!(Session::open(deepest).is_ok());
    assert!(matches!(
        current.fork(root.path(), ForkBoundary::Latest),
        Err(ForkError::LineageTooDeep)
    ));
}

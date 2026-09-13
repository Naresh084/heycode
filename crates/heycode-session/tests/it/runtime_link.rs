//! R06 durable provider-native runtime session linkage.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{Session, SessionEventKind, derive_messages};

#[test]
fn runtime_link_is_v2_insert_once_resume_visible_and_not_model_visible() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let event = session
        .append(SessionEventKind::RuntimeLinked {
            runtime: "codex".to_owned(),
            runtime_session_id: "thread-123".to_owned(),
        })
        .unwrap();
    assert_eq!(event.kind.name(), "runtime/linked");
    assert_eq!(session.runtime_link(), Some(("codex", "thread-123")));
    assert!(derive_messages(session.events()).is_empty());
    assert!(
        session
            .append(SessionEventKind::RuntimeLinked {
                runtime: "codex".to_owned(),
                runtime_session_id: "thread-other".to_owned(),
            })
            .is_err()
    );
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let resumed = Session::open(directory).unwrap();
    assert_eq!(resumed.runtime_link(), Some(("codex", "thread-123")));
}

#[test]
fn runtime_link_rejects_v1_and_invalid_external_identity() {
    let root = tempfile::tempdir().unwrap();
    let v1 = root.path().join("v1-runtime-link");
    std::fs::create_dir(&v1).unwrap();
    std::fs::write(
        v1.join("session.jsonl"),
        "{\"v\":1,\"seq\":0,\"time_ms\":1,\"kind\":\"runtime/linked\",\"data\":{\"runtime\":\"codex\",\"runtime_session_id\":\"thread-1\"}}\n",
    )
    .unwrap();
    assert!(matches!(
        Session::open(v1),
        Err(heycode_session::OpenError::UnknownKind { .. })
    ));

    let mut session = Session::create(root.path()).unwrap();
    assert!(
        session
            .append(SessionEventKind::RuntimeLinked {
                runtime: "Codex".to_owned(),
                runtime_session_id: "bad\nthread".to_owned(),
            })
            .is_err()
    );
}

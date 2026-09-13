//! C09 SQLite projection contracts: JSONL remains the only durable truth.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    Session, SessionEventKind, SessionIndexComparison, SessionIndexError, SessionIndexIssue,
    SqliteSessionIndex,
};
use sha2::{Digest as _, Sha256};

fn seed_session(root: &std::path::Path, text: &str) -> Session {
    let mut session = Session::create(root).unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: text.to_owned(),
        })
        .unwrap();
    session.flush().unwrap();
    session
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn rebuild_is_deterministic_and_compare_rederives_jsonl_truth() {
    let root = tempfile::tempdir().unwrap();
    let first = seed_session(root.path(), "first durable prompt");
    let second = seed_session(root.path(), "second durable prompt");
    drop(first);
    drop(second);

    let index = SqliteSessionIndex::new(root.path().to_path_buf()).unwrap();
    assert_eq!(
        index.compare().unwrap(),
        SessionIndexComparison::RebuildRequired(SessionIndexIssue::Missing)
    );

    let first_snapshot = index.rebuild().unwrap();
    assert_eq!(first_snapshot.session_count(), 2);
    assert_eq!(
        index.compare().unwrap(),
        SessionIndexComparison::Current(first_snapshot.clone())
    );
    let first_bytes = std::fs::read(index.path()).unwrap();
    assert!(first_bytes.starts_with(b"SQLite format 3\0"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(index.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let second_snapshot = index.rebuild().unwrap();
    let second_bytes = std::fs::read(index.path()).unwrap();
    assert_eq!(second_snapshot, first_snapshot);
    assert_eq!(
        sha256(&second_bytes),
        sha256(&first_bytes),
        "the same JSONL generation must produce the same SQLite bytes"
    );
}

#[test]
fn append_makes_the_projection_stale_until_a_rebuild() {
    let root = tempfile::tempdir().unwrap();
    let mut session = seed_session(root.path(), "before index");
    let directory = session.path().parent().unwrap().to_path_buf();
    let index = SqliteSessionIndex::new(root.path().to_path_buf()).unwrap();
    let before = index.rebuild().unwrap();

    session
        .append(SessionEventKind::AssistantMessage {
            turn: 1,
            step: 1,
            content: "after index".to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: None,
        })
        .unwrap();
    session.flush().unwrap();
    assert_eq!(
        index.compare().unwrap(),
        SessionIndexComparison::RebuildRequired(SessionIndexIssue::Stale)
    );

    let reopened = Session::open(&directory).unwrap();
    assert_eq!(
        reopened.events().len(),
        2,
        "JSONL remains independently usable"
    );
    drop(reopened);
    drop(session);
    let after = index.rebuild().unwrap();
    assert_ne!(after.manifest_sha256(), before.manifest_sha256());
    assert_eq!(
        index.compare().unwrap(),
        SessionIndexComparison::Current(after)
    );
}

#[test]
fn corrupt_and_interrupted_indexes_are_detected_and_rebuildable() {
    let root = tempfile::tempdir().unwrap();
    drop(seed_session(
        root.path(),
        "truth survives every index failure",
    ));
    let index = SqliteSessionIndex::new(root.path().to_path_buf()).unwrap();
    let expected = index.rebuild().unwrap();

    std::fs::write(index.path(), b"not sqlite").unwrap();
    assert_eq!(
        index.compare().unwrap(),
        SessionIndexComparison::RebuildRequired(SessionIndexIssue::Corrupt)
    );
    assert_eq!(index.rebuild().unwrap(), expected);

    let residue = root.path().join(".session-index-rebuild-crashed.tmp");
    std::fs::write(&residue, b"SQLite format 3\0partial").unwrap();
    assert_eq!(
        index.compare().unwrap(),
        SessionIndexComparison::RebuildRequired(SessionIndexIssue::UnfinishedRebuild)
    );
    assert_eq!(index.rebuild().unwrap(), expected);
    assert!(
        !residue.exists(),
        "rebuild removes only validated index residue"
    );
}

#[test]
fn invalid_jsonl_aborts_before_replacing_the_last_good_index() {
    let root = tempfile::tempdir().unwrap();
    let session = seed_session(root.path(), "last good truth");
    let log = session.path().to_path_buf();
    drop(session);
    let index = SqliteSessionIndex::new(root.path().to_path_buf()).unwrap();
    let snapshot = index.rebuild().unwrap();
    let prior_index = std::fs::read(index.path()).unwrap();

    use std::io::Write as _;
    let mut log_file = std::fs::OpenOptions::new().append(true).open(log).unwrap();
    log_file.write_all(b"{torn").unwrap();
    log_file.flush().unwrap();

    assert!(matches!(
        index.rebuild(),
        Err(SessionIndexError::InvalidTruth(_))
    ));
    assert_eq!(std::fs::read(index.path()).unwrap(), prior_index);
    assert_eq!(snapshot.session_count(), 1);
}

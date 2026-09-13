//! Q17 saved-session migration matrix and downgrade guidance.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    CURRENT_SESSION_LOG_VERSION, OpenError, Session, SessionDowngradeGuidance, SessionEventKind,
};

fn seeded(raw: &str, name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(name);
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("session.jsonl"), raw).unwrap();
    (root, directory)
}

#[test]
fn every_supported_saved_session_generation_reopens_without_rewrite() {
    let cases = [
        (
            "v1",
            include_str!("../fixtures/session-v1.jsonl"),
            1_u8,
            None,
        ),
        (
            "v2",
            include_str!("../fixtures/session-v2.jsonl"),
            2_u8,
            Some(0_u64),
        ),
        (
            "mixed-v1-v2",
            include_str!("../fixtures/session-v1-v2.jsonl"),
            2_u8,
            Some(12_u64),
        ),
    ];

    for (name, fixture, latest_version, first_v2_seq) in cases {
        let (_root, directory) = seeded(fixture, name);
        let before = std::fs::read(directory.join("session.jsonl")).unwrap();
        let opened = Session::open(&directory).unwrap();
        assert_eq!(opened.events().last().unwrap().v, latest_version);
        assert_eq!(
            opened
                .downgrade_guidance(CURRENT_SESSION_LOG_VERSION)
                .unwrap(),
            SessionDowngradeGuidance::Compatible {
                reader_max_version: CURRENT_SESSION_LOG_VERSION,
            }
        );
        match first_v2_seq {
            None => assert_eq!(
                opened.downgrade_guidance(1).unwrap(),
                SessionDowngradeGuidance::Compatible {
                    reader_max_version: 1,
                }
            ),
            Some(first_incompatible_seq) => assert_eq!(
                opened.downgrade_guidance(1).unwrap(),
                SessionDowngradeGuidance::RestorePreUpgradeCopyOrUseNewerBinary {
                    reader_max_version: 1,
                    first_incompatible_seq,
                    required_version: 2,
                }
            ),
        }
        drop(opened);
        let reopened = Session::open(&directory).unwrap();
        drop(reopened);
        assert_eq!(
            std::fs::read(directory.join("session.jsonl")).unwrap(),
            before
        );
    }
}

#[test]
fn v1_upgrade_is_append_only_and_repeated_open_is_idempotent() {
    let fixture = include_str!("../fixtures/session-v1.jsonl");
    let (_root, directory) = seeded(fixture, "append-only-upgrade");
    let mut session = Session::open(&directory).unwrap();
    session
        .append(SessionEventKind::SessionTitle {
            title: "v2 append".to_owned(),
        })
        .unwrap();
    session.flush().unwrap();
    assert_eq!(
        session.downgrade_guidance(1).unwrap(),
        SessionDowngradeGuidance::RestorePreUpgradeCopyOrUseNewerBinary {
            reader_max_version: 1,
            first_incompatible_seq: 12,
            required_version: 2,
        }
    );
    drop(session);

    let migrated = std::fs::read_to_string(directory.join("session.jsonl")).unwrap();
    assert!(migrated.starts_with(fixture), "historical v1 bytes changed");
    assert_eq!(migrated.lines().count(), 13);
    drop(Session::open(&directory).unwrap());
    drop(Session::open(&directory).unwrap());
    assert_eq!(
        std::fs::read_to_string(directory.join("session.jsonl")).unwrap(),
        migrated
    );
}

#[test]
fn future_session_version_is_refused_unchanged_with_safe_guidance() {
    let fixture = include_str!("../fixtures/session-v3-newer.jsonl");
    let (_root, directory) = seeded(fixture, "future-session");
    let error = Session::open(&directory)
        .err()
        .expect("future session must be refused");
    assert!(matches!(
        error,
        OpenError::UnsupportedVersion {
            found: 3,
            minimum: 1,
            maximum: 2,
        }
    ));
    let message = error.to_string();
    assert!(
        message.contains("use a heycode build supporting this version"),
        "{message}"
    );
    assert!(message.contains("was not modified"), "{message}");
    assert_eq!(
        std::fs::read_to_string(directory.join("session.jsonl")).unwrap(),
        fixture
    );
}

#[test]
fn session_activation_is_v2_only() {
    let raw = "{\"v\":1,\"seq\":0,\"time_ms\":0,\"kind\":\"session/activated\",\"data\":{}}\n";
    let (_root, directory) = seeded(raw, "activation-v1");
    assert!(
        matches!(Session::open(&directory), Err(OpenError::UnknownKind { kind, .. }) if kind == "session/activated")
    );
}

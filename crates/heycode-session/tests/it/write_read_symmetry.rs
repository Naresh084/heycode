//! The write path must refuse exactly what the read path refuses.
//!
//! A durable append-only log is never repaired in place (AGENTS.md §"never
//! truncate or rewrite the durable log"), so any line the writer commits and
//! `Session::open` later rejects makes that session unopenable forever. Every
//! postcondition `open` enforces over the whole log therefore has to hold at
//! the commit point too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    AttachmentContentId, AttachmentDimensions, AttachmentMediaType, AttachmentMetadata,
};
use heycode_session::{AppendError, OpenError, Session, SessionEventKind};

fn attachment() -> AttachmentMetadata {
    AttachmentMetadata::new(
        AttachmentContentId::from_sha256([0x42; 32]),
        AttachmentMediaType::new("image/png").unwrap(),
        100,
        Some("image.png".to_owned()),
        Some(AttachmentDimensions::new(10, 10).unwrap()),
    )
    .unwrap()
}

fn admit(session: &mut Session, attachment: &AttachmentMetadata) {
    session
        .append(SessionEventKind::AttachmentAdded {
            attachment: Box::new(attachment.clone()),
        })
        .unwrap();
}

fn selection(attachment: &AttachmentMetadata) -> SessionEventKind {
    SessionEventKind::UserAttachments {
        attachments: vec![attachment.clone()],
        document_routes: Vec::new(),
    }
}

#[test]
fn raw_append_refuses_the_pair_owned_attachment_selection() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let attachment = attachment();
    admit(&mut session, &attachment);

    let error = session.append(selection(&attachment)).unwrap_err();
    assert!(
        matches!(&error, AppendError::InvalidEvent { message }
            if message.contains("user/message")),
        "{error:?}"
    );

    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(&directory).unwrap();
    assert_eq!(
        reopened.events().len(),
        1,
        "the refused selection must never have reached durable storage"
    );
}

#[test]
fn raw_append_refuses_an_unadmitted_selection_before_it_reaches_disk() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();

    let error = session.append(selection(&attachment())).unwrap_err();
    assert!(
        matches!(&error, AppendError::InvalidEvent { .. }),
        "{error:?}"
    );

    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    assert!(Session::open(&directory).unwrap().events().is_empty());
}

#[test]
fn the_atomic_pair_survives_a_following_unrelated_append() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let attachment = attachment();
    admit(&mut session, &attachment);
    session
        .append_user_message_with_attachments("look", vec![attachment])
        .unwrap();
    session
        .append(SessionEventKind::TurnStart { turn: 1 })
        .unwrap();

    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    assert_eq!(Session::open(&directory).unwrap().events().len(), 4);
}

#[test]
fn append_refuses_a_line_that_would_push_the_log_past_the_reader_byte_bound() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let oversized = "y".repeat(64 * 1024 * 1024 + 4096);

    // The refused payload is 64 MiB; never render it into a panic message.
    let error = match session.append(SessionEventKind::UserMessage { text: oversized }) {
        Ok(_) => panic!("an unopenable log was committed"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, AppendError::LogFull { maximum_bytes } if *maximum_bytes == 64 * 1024 * 1024),
        "{error:?}"
    );

    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    assert!(
        Session::open(&directory).unwrap().events().is_empty(),
        "a log the writer refused must still open"
    );
}

#[test]
fn a_newer_writable_handle_supersedes_the_older_one_instead_of_duplicating_seq() {
    let root = tempfile::tempdir().unwrap();
    let directory = {
        let session = Session::create(root.path()).unwrap();
        session.path().parent().unwrap().to_path_buf()
    };

    let mut first = Session::open_for_writing(&directory).unwrap();
    let mut second = Session::open_for_writing(&directory).unwrap();

    // Both handles replayed the same empty log, so both would mint seq 0.
    // Only the current lease holder is allowed to.
    let error = first
        .append(SessionEventKind::SessionTitle {
            title: "stale writer".to_owned(),
        })
        .unwrap_err();
    assert!(matches!(&error, AppendError::Superseded), "{error:?}");
    second
        .append(SessionEventKind::SessionTitle {
            title: "live writer".to_owned(),
        })
        .unwrap();

    drop(first);
    drop(second);
    assert_eq!(Session::open(&directory).unwrap().events().len(), 1);
}

#[test]
fn a_read_only_open_never_takes_the_writer_lease() {
    let root = tempfile::tempdir().unwrap();
    let directory = {
        let session = Session::create(root.path()).unwrap();
        session.path().parent().unwrap().to_path_buf()
    };

    // Listing, export and repair all go through `Session::open`; it must not
    // disturb — or be disturbed by — the session's writer.
    let mut writer = Session::open_for_writing(&directory).unwrap();
    let reader = Session::open(&directory).unwrap();
    assert!(reader.events().is_empty());
    writer
        .append(SessionEventKind::SessionTitle {
            title: "still writable".to_owned(),
        })
        .unwrap();
}

#[test]
fn a_resume_that_fails_to_open_leaves_the_live_writer_current() {
    use std::io::Write as _;

    let root = tempfile::tempdir().unwrap();
    let mut live = Session::create(root.path()).unwrap();
    let directory = live.path().parent().unwrap().to_path_buf();

    // A torn tail is what a crashed append leaves behind: `Session::open`
    // refuses it, so this resume fails after it reached for the lease.
    std::fs::OpenOptions::new()
        .append(true)
        .open(live.path())
        .unwrap()
        .write_all(b"{\"v\":2")
        .unwrap();

    let error = match Session::open_for_writing(&directory) {
        Ok(_) => panic!("a torn tail must not open for writing"),
        Err(error) => error,
    };
    assert!(matches!(&error, OpenError::UnterminatedTail), "{error:?}");

    // A lease is published only once the handle that holds it exists, so the
    // failed resume never superseded the writer that was already there.
    live.append(SessionEventKind::SessionTitle {
        title: "still mine".to_owned(),
    })
    .unwrap();
}

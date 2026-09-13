//! C08 — what a killed process leaves behind, read back off real storage.
//!
//! Every case here builds a session on disk, stops writing where a crash
//! would, and reopens it, so the claims hold against the reader and the
//! filesystem rather than against a hand-built event slice.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use heycode_session::{
    OpenError, OpenOutcome, OpenRecordKind, Role, Session, SessionEventKind, ToolCallOut,
    TurnEndReason, derive_messages, derive_messages_repaired, project_repair,
};
use tempfile::TempDir;

fn assistant_calling(turn: u64, calls: &[(&str, &str)]) -> SessionEventKind {
    SessionEventKind::AssistantMessage {
        turn,
        step: 0,
        content: String::new(),
        reasoning: None,
        tool_calls: Some(
            calls
                .iter()
                .map(|(id, name)| ToolCallOut {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                    arguments: "{}".to_owned(),
                })
                .collect(),
        ),
        usage: None,
    }
}

fn dispatch(turn: u64, id: &str, name: &str) -> SessionEventKind {
    SessionEventKind::ToolCall {
        turn,
        call_id: heycode_core::CallId::from_raw(id),
        name: name.to_owned(),
        args: serde_json::json!({}),
    }
}

/// Write a session, then stop as a `kill -9` would: no closing events, and the
/// handle dropped without further appends. Returns the session directory.
fn crashed_session(root: &Path, kinds: Vec<SessionEventKind>) -> PathBuf {
    let mut session = Session::create(root).unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    for kind in kinds {
        session.append(kind).unwrap();
    }
    drop(session);
    directory
}

#[test]
fn a_session_killed_mid_tool_call_still_opens_because_an_open_record_is_not_a_corrupt_one() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            SessionEventKind::StepStart { turn: 0, step: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
        ],
    );

    let resumed = Session::open(&directory).expect("an unclosed record is not a read failure");
    assert_eq!(resumed.events().len(), 4);
}

#[test]
fn a_session_killed_mid_tool_call_reports_the_call_open_with_an_unknown_outcome() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            SessionEventKind::StepStart { turn: 0, step: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
        ],
    );
    let repair = project_repair(Session::open(&directory).unwrap().events());

    assert!(!repair.is_clean());
    let kinds = repair
        .open()
        .iter()
        .map(|record| (record.kind().clone(), record.outcome()))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            (OpenRecordKind::Turn { turn: 0 }, OpenOutcome::Unknown),
            (
                OpenRecordKind::Step { turn: 0, step: 0 },
                OpenOutcome::Unknown
            ),
            (
                OpenRecordKind::ToolCall {
                    turn: 0,
                    call_id: heycode_core::CallId::from_raw("call_1"),
                    name: "bash".to_owned(),
                    dispatched: true,
                },
                OpenOutcome::Unknown
            ),
        ],
        "the turn, its step and its call are each open, and none of them succeeded"
    );
}

#[test]
fn a_completed_session_has_nothing_open_so_repair_reports_nothing() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            SessionEventKind::StepStart { turn: 0, step: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
            SessionEventKind::ToolResult {
                call_id: heycode_core::CallId::from_raw("call_1"),
                content: "ok".to_owned(),
                is_error: false,
                untrusted_content: None,
            },
            SessionEventKind::StepEnd { turn: 0, step: 0 },
            SessionEventKind::TurnEnd {
                turn: 0,
                reason: TurnEndReason::Stop,
            },
        ],
    );
    assert!(project_repair(Session::open(&directory).unwrap().events()).is_clean());
}

#[test]
fn projecting_repair_writes_nothing_back_to_the_log() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
        ],
    );
    let log = directory.join("session.jsonl");
    let before = std::fs::read(&log).unwrap();

    let session = Session::open(&directory).unwrap();
    let first = project_repair(session.events());
    let second = project_repair(session.events());
    drop(session);

    assert_eq!(
        std::fs::read(&log).unwrap(),
        before,
        "JSONL is the truth; repair reads it and leaves it exactly as it found it"
    );
    assert_eq!(first, second, "repeating the projection changes nothing");
    assert_eq!(
        project_repair(Session::open(&directory).unwrap().events()),
        first,
        "and reopening from disk yields the same report"
    );
}

#[test]
fn a_second_crash_adds_one_open_turn_rather_than_compounding_the_first() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
        ],
    );

    // Resume and crash again inside the next turn.
    let mut resumed = Session::open(&directory).unwrap();
    resumed
        .append(SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    resumed
        .append(assistant_calling(1, &[("call_2", "read")]))
        .unwrap();
    resumed.append(dispatch(1, "call_2", "read")).unwrap();
    drop(resumed);

    let repair = project_repair(Session::open(&directory).unwrap().events());
    let outcomes = repair
        .open()
        .iter()
        .map(|record| (record.kind().turn(), record.outcome()))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![
            (0, OpenOutcome::Unknown),
            (0, OpenOutcome::Unknown),
            (1, OpenOutcome::Unknown),
            (1, OpenOutcome::Unknown),
        ],
        "two crashes leave two open turns and two unanswered calls, counted once each"
    );
    assert_eq!(repair.unanswered_tool_calls().len(), 2);
}

#[test]
fn a_torn_final_line_is_an_unterminated_tail_not_an_invalid_event() {
    // Classification matters at runtime: the query backend retries a tail or
    // corrupt-line error because a live writer produces the same evidence, and
    // does not retry an invalid event. A truncated write is the retryable
    // class, and it must never be mistaken for a semantically bad record.
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
        ],
    );
    let log = directory.join("session.jsonl");
    let raw = std::fs::read_to_string(&log).unwrap();
    let last_line_start = raw[..raw.len() - 1].rfind('\n').unwrap() + 1;
    let torn = &raw[..last_line_start + 20];
    assert!(!torn.ends_with('\n'), "the tear must land inside a line");
    std::fs::write(&log, torn).unwrap();

    match Session::open(&directory) {
        Err(OpenError::UnterminatedTail) => {}
        Err(other) => panic!("a torn write must classify as UnterminatedTail, got {other:?}"),
        Ok(_) => panic!("a torn write must not open as if the line had committed"),
    }
}

#[test]
fn a_torn_final_line_never_reaches_the_projection_as_a_record() {
    // The bytes after the last newline belong to an append that never returned
    // to its caller and was never published on the bus. The reader refuses the
    // log outright, so no projection can mistake the debris for work.
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            assistant_calling(0, &[("call_1", "bash")]),
        ],
    );
    let log = directory.join("session.jsonl");
    let raw = std::fs::read_to_string(&log).unwrap();
    let intact = &raw[..raw[..raw.len() - 1].rfind('\n').unwrap() + 1];
    let complete = project_repair(Session::open(&directory).unwrap().events())
        .open()
        .len();

    // Re-tear the same log so only the first line survives whole.
    std::fs::write(
        &log,
        format!("{intact}{{\"v\":2,\"seq\":2,\"kind\":\"tool/ca"),
    )
    .unwrap();
    assert!(Session::open(&directory).is_err());

    // And the complete prefix on its own reports only what it actually holds.
    std::fs::write(&log, intact).unwrap();
    let after_tear = project_repair(Session::open(&directory).unwrap().events());
    assert_eq!(after_tear.open().len(), 1, "only the open turn is a record");
    assert!(after_tear.unanswered_tool_calls().is_empty());
    assert_eq!(complete, 2, "the untorn log held the turn and its call");
}

#[test]
fn a_well_formed_record_with_no_closing_event_is_reported_rather_than_refused() {
    // The contrast with a torn line: nothing here is damaged, so the reader
    // must not treat an open turn as a read failure.
    let root = TempDir::new().unwrap();
    let directory = crashed_session(root.path(), vec![SessionEventKind::TurnStart { turn: 0 }]);
    let session = Session::open(&directory).expect("an open turn is a fact, not a corruption");
    let repair = project_repair(session.events());
    assert_eq!(repair.open().len(), 1);
    assert_eq!(repair.open()[0].kind(), &OpenRecordKind::Turn { turn: 0 });
}

#[test]
fn replay_after_a_crash_closes_the_unanswered_call_as_interrupted_instead_of_dropping_it() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            SessionEventKind::UserMessage {
                text: "delete the file".to_owned(),
            },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
        ],
    );
    let session = Session::open(&directory).unwrap();

    let faithful = derive_messages(session.events());
    assert_eq!(
        faithful.len(),
        2,
        "the faithful fold reports the log as it is: the call stays unanswered"
    );

    let repaired = derive_messages_repaired(session.events());
    assert_eq!(repaired.len(), 3, "the call is answered, not dropped");
    let closing = &repaired[2];
    assert_eq!(closing.role, Role::Tool);
    assert_eq!(
        closing
            .tool_call_id
            .as_ref()
            .map(heycode_core::CallId::as_str),
        Some("call_1"),
        "the repair answers the exact call the model made"
    );
    assert_eq!(
        closing.tool_result_is_error,
        Some(true),
        "an unknown outcome never enters the conversation as a success"
    );
    assert!(
        closing.content.contains("interrupted") && closing.content.contains("unknown"),
        "the model must be told what actually happened: {}",
        closing.content
    );

    assert_eq!(
        repaired[..2],
        faithful[..],
        "repair adds the missing answer and changes nothing else"
    );
}

#[test]
fn every_call_in_a_partially_answered_block_is_answered_before_the_block_ends() {
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            assistant_calling(0, &[("call_1", "bash"), ("call_2", "read")]),
            dispatch(0, "call_1", "bash"),
            SessionEventKind::ToolResult {
                call_id: heycode_core::CallId::from_raw("call_1"),
                content: "total 0".to_owned(),
                is_error: false,
                untrusted_content: None,
            },
            dispatch(0, "call_2", "read"),
            SessionEventKind::UserMessage {
                text: "never mind".to_owned(),
            },
        ],
    );
    let session = Session::open(&directory).unwrap();
    let repaired = derive_messages_repaired(session.events());

    let roles = repaired
        .iter()
        .map(|message| message.role)
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec![Role::Assistant, Role::Tool, Role::Tool, Role::User],
        "the interrupted answer lands before the user's next message, not after it"
    );
    assert_eq!(
        repaired[1].tool_result_is_error,
        Some(false),
        "the call that really finished keeps its real outcome"
    );
    assert_eq!(repaired[1].content, "total 0");
    assert_eq!(
        repaired[2]
            .tool_call_id
            .as_ref()
            .map(heycode_core::CallId::as_str),
        Some("call_2")
    );
    assert_eq!(repaired[2].tool_result_is_error, Some(true));
}

#[test]
fn repaired_replay_is_identical_to_the_faithful_fold_on_a_healthy_log() {
    // The property that lets a consumer call the repairing fold every time.
    let root = TempDir::new().unwrap();
    let directory = crashed_session(
        root.path(),
        vec![
            SessionEventKind::TurnStart { turn: 0 },
            SessionEventKind::UserMessage {
                text: "list files".to_owned(),
            },
            assistant_calling(0, &[("call_1", "bash")]),
            dispatch(0, "call_1", "bash"),
            SessionEventKind::ToolResult {
                call_id: heycode_core::CallId::from_raw("call_1"),
                content: "a.txt".to_owned(),
                is_error: false,
                untrusted_content: None,
            },
            SessionEventKind::AssistantMessage {
                turn: 0,
                step: 1,
                content: "There is one file.".to_owned(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
            SessionEventKind::TurnEnd {
                turn: 0,
                reason: TurnEndReason::Stop,
            },
        ],
    );
    let session = Session::open(&directory).unwrap();
    assert_eq!(
        derive_messages_repaired(session.events()),
        derive_messages(session.events())
    );
}

#[test]
fn a_forked_child_inherits_the_parents_repair_report_over_its_shared_prefix() {
    // Fork refuses an open turn, so the prefix a child can inherit is a closed
    // one; what it inherits must still report the calls that turn abandoned.
    let root = TempDir::new().unwrap();
    let mut parent = Session::create(root.path()).unwrap();
    for kind in [
        SessionEventKind::TurnStart { turn: 0 },
        assistant_calling(0, &[("call_1", "bash")]),
        dispatch(0, "call_1", "bash"),
        SessionEventKind::TurnEnd {
            turn: 0,
            reason: TurnEndReason::Aborted,
        },
    ] {
        parent.append(kind).unwrap();
    }
    let child = parent
        .fork(root.path(), heycode_session::ForkBoundary::Latest)
        .unwrap();

    let repair = project_repair(child.events());
    assert_eq!(repair.unanswered_tool_calls().len(), 1);
    assert_eq!(
        repair.open()[0].outcome(),
        OpenOutcome::Interrupted,
        "the turn ended without the call, so nothing can answer it now"
    );
}

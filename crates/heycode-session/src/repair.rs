//! C08 — crash repair: the records a killed process left open.
//!
//! JSONL is the truth in this architecture, so repair here is a **read-time
//! projection over an untouched log**, exactly like [`crate::project_usage`]
//! and [`crate::derive_messages`]. Nothing in this module writes.
//!
//! The argument against writing repair records back is that they would be
//! indistinguishable from real ones. A `turn/end` appended at resume time
//! carries the resume clock in `time_ms` and one of the four ordinary
//! [`crate::TurnEndReason`] values, so no later reader — no export, no index
//! rebuild, no support bundle — could tell it from a `turn/end` the agent
//! wrote while the turn was actually ending. That is not a repair, it is a
//! fabricated durable fact, which is the failure this row exists to prevent.
//! The absence of a closing record *is* the honest record of a crash.
//!
//! Three consequences reinforce the choice. Listing the store opens every
//! session, so a writing repair would rewrite every crashed log merely to draw
//! a picker. A log may be open by a live process — that is why
//! `LocalSessionQueryBackend` retries a tail error rather than condemning the
//! session — and appending into it would collide with the writer's sequence.
//! And [`crate::ForkError::OpenTurn`] deliberately refuses to fork across an
//! open turn; a synthetic `turn/end` would silently convert that refusal into
//! an accepted fork at an invented boundary.
//!
//! Idempotence is therefore structural rather than defended: [`project_repair`]
//! is a pure function of the event slice, so running it twice cannot compound,
//! and a second crash adds a second open record rather than doubling the first.
//!
//! A **torn final line** is deliberately not this module's business. Bytes
//! after the last newline are an append that never returned to its caller and
//! was never published on the bus, so they are storage debris, not a record —
//! [`crate::Session::open`] refuses them as
//! [`OpenError::UnterminatedTail`](crate::OpenError::UnterminatedTail) and the
//! projection therefore never sees them. That classification is load-bearing:
//! a live concurrent writer produces byte-identical evidence, which is why the
//! query path treats a tail error as possibly transient and retries it, while
//! [`OpenError::InvalidEvent`](crate::OpenError::InvalidEvent) is permanent and
//! is not retried. Discarding the debris would also be a write, and the reader
//! must not write — least of all into a log another process may still own.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::{SessionEvent, SessionEventKind};

/// What the log is entitled to say about a record it never closed.
///
/// Neither value is success. The distinction is only how much the log proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    /// The log ends while this record is still open. Nothing recorded an
    /// outcome, and the log alone cannot separate "the process was killed
    /// here" from "the session is still running" — so the outcome is unknown.
    Unknown,
    /// This record's own turn later ended without it. Nothing can close it
    /// now, so the work it represents was interrupted and whatever it did
    /// before that is unrecorded.
    Interrupted,
}

/// Which kind of work the log opened and never closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenRecordKind {
    /// A `turn/start` with no matching `turn/end`.
    Turn {
        /// Zero-based turn index.
        turn: u64,
    },
    /// A `step/start` with no matching `step/end`.
    Step {
        /// Turn containing the step.
        turn: u64,
        /// Zero-based step index within the turn.
        step: u32,
    },
    /// A tool call the model requested that no `tool/result` ever answered.
    ToolCall {
        /// Turn the call belongs to.
        turn: u64,
        /// Provider call id.
        call_id: heycode_core::CallId,
        /// Tool name as requested.
        name: String,
        /// Whether a `tool/call` dispatch reached the log before the crash.
        ///
        /// `false` means the model asked for the call and the process died
        /// before heycode even recorded dispatching it, so the tool certainly
        /// never ran through this path; `true` means it was dispatched and the
        /// outcome is genuinely unknown. Neither is a completed call.
        dispatched: bool,
    },
    /// A provider-executed call with no matching `server-tool/result`.
    ServerToolCall {
        /// Turn the call belongs to.
        turn: u64,
        /// Step the call belongs to.
        step: u32,
        /// Provider call id.
        call_id: heycode_core::CallId,
    },
}

impl OpenRecordKind {
    /// Turn this record belongs to; the scope whose closure settles it.
    #[must_use]
    pub const fn turn(&self) -> u64 {
        match self {
            Self::Turn { turn }
            | Self::Step { turn, .. }
            | Self::ToolCall { turn, .. }
            | Self::ServerToolCall { turn, .. } => *turn,
        }
    }
}

/// One record the log opened and never closed, and what may be said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRecord {
    seq: u64,
    kind: OpenRecordKind,
    outcome: OpenOutcome,
}

impl OpenRecord {
    /// Sequence of the event that opened this record.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// What was left open.
    #[must_use]
    pub const fn kind(&self) -> &OpenRecordKind {
        &self.kind
    }

    /// How much the log proves about the missing outcome.
    #[must_use]
    pub const fn outcome(&self) -> OpenOutcome {
        self.outcome
    }
}

/// Everything one durable log left open, in the order it was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRepair {
    open: Vec<OpenRecord>,
}

impl SessionRepair {
    /// Open records in log order, oldest first.
    #[must_use]
    pub fn open(&self) -> &[OpenRecord] {
        &self.open
    }

    /// Whether the log closed everything it opened.
    ///
    /// `false` on a session that is merely running right now, because a live
    /// open turn and a crashed one are the same evidence. Read it with
    /// [`OpenRecord::outcome`] rather than as "this session is damaged".
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.open.is_empty()
    }

    /// Tool calls no `tool/result` ever answered, in the order they were
    /// requested.
    ///
    /// A superset of what [`crate::derive_messages_repaired`] closes on the
    /// wire: that fold answers the calls an assistant message actually put in
    /// front of the model, so a dispatch with no surviving declaration, or one
    /// a compaction shadowed, is reported here and has nothing on the wire to
    /// answer.
    #[must_use]
    pub fn unanswered_tool_calls(&self) -> Vec<&OpenRecord> {
        self.open
            .iter()
            .filter(|record| matches!(record.kind, OpenRecordKind::ToolCall { .. }))
            .collect()
    }
}

/// Project every open call, step and turn out of a durable event slice.
///
/// Pure and total: it reads only what was written, reports what has no
/// recorded outcome, and never infers one from an adjacent event. A tool call
/// followed by a successful-looking assistant message is still an unanswered
/// call.
///
/// Classification is turn-scoped and uniform across kinds, and claims only
/// what the log proves: a record is [`OpenOutcome::Interrupted`] when a later
/// `turn/end` closed its own turn without it, because no outcome can reach it
/// after that. Everything else is [`OpenOutcome::Unknown`].
///
/// A later `turn/start` for a *different* turn is deliberately not treated as
/// evidence. The envelope permits interleaved turns — [`crate::project_usage`]
/// attributes by turn index precisely because their events need not be
/// contiguous — so a new turn beginning does not prove an older one stopped.
/// Reading it as proof would make this projection assert an abandonment the
/// log never recorded, which is the same class of invention as asserting a
/// success.
///
/// Compaction does not shadow anything here. This is a report about the log,
/// not about what a provider sees, and a compacted range still had a crash in
/// it.
#[must_use]
pub fn project_repair(events: &[SessionEvent]) -> SessionRepair {
    let mut slots: Vec<Option<OpenSlot>> = Vec::new();
    let mut turns: BTreeMap<u64, VecDeque<usize>> = BTreeMap::new();
    let mut steps: BTreeMap<(u64, u32), VecDeque<usize>> = BTreeMap::new();
    let mut tool_calls: HashMap<heycode_core::CallId, VecDeque<usize>> = HashMap::new();
    let mut server_calls: HashMap<heycode_core::CallId, VecDeque<usize>> = HashMap::new();
    let mut turn_ends = TurnEnds::default();

    for event in events {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => {
                open_slot(
                    &mut slots,
                    turns.entry(*turn).or_default(),
                    event.seq,
                    OpenRecordKind::Turn { turn: *turn },
                );
            }
            SessionEventKind::TurnEnd { turn, .. } => {
                turn_ends.record(*turn, event.seq);
                close_slot(&mut slots, turns.get_mut(turn));
            }
            SessionEventKind::StepStart { turn, step } => open_slot(
                &mut slots,
                steps.entry((*turn, *step)).or_default(),
                event.seq,
                OpenRecordKind::Step {
                    turn: *turn,
                    step: *step,
                },
            ),
            SessionEventKind::StepEnd { turn, step } => {
                close_slot(&mut slots, steps.get_mut(&(*turn, *step)));
            }
            SessionEventKind::AssistantMessage {
                turn,
                tool_calls: Some(requested),
                ..
            } => {
                for call in requested {
                    let call_id = heycode_core::CallId::from_raw(call.id.clone());
                    open_slot(
                        &mut slots,
                        tool_calls.entry(call_id.clone()).or_default(),
                        event.seq,
                        OpenRecordKind::ToolCall {
                            turn: *turn,
                            call_id,
                            name: call.name.clone(),
                            dispatched: false,
                        },
                    );
                }
            }
            SessionEventKind::ToolCall {
                turn,
                call_id,
                name,
                ..
            } => {
                let queue = tool_calls.entry(call_id.clone()).or_default();
                // The dispatch belongs to the oldest still-undispatched
                // declaration of this id; a dispatch with no declaration opens
                // its own record rather than being dropped.
                let existing = queue.iter().copied().find(|index| {
                    matches!(
                        slots
                            .get(*index)
                            .and_then(Option::as_ref)
                            .map(|slot| &slot.kind),
                        Some(OpenRecordKind::ToolCall {
                            dispatched: false,
                            ..
                        })
                    )
                });
                match existing {
                    Some(index) => {
                        if let Some(slot) = slots.get_mut(index).and_then(Option::as_mut)
                            && let OpenRecordKind::ToolCall { dispatched, .. } = &mut slot.kind
                        {
                            *dispatched = true;
                        }
                    }
                    None => open_slot(
                        &mut slots,
                        queue,
                        event.seq,
                        OpenRecordKind::ToolCall {
                            turn: *turn,
                            call_id: call_id.clone(),
                            name: name.clone(),
                            dispatched: true,
                        },
                    ),
                }
            }
            SessionEventKind::ToolResult { call_id, .. }
            | SessionEventKind::RichToolResult { call_id, .. } => {
                close_slot(&mut slots, tool_calls.get_mut(call_id));
            }
            SessionEventKind::ServerToolCall {
                turn, step, call, ..
            } => open_slot(
                &mut slots,
                server_calls.entry(call.id().clone()).or_default(),
                event.seq,
                OpenRecordKind::ServerToolCall {
                    turn: *turn,
                    step: *step,
                    call_id: call.id().clone(),
                },
            ),
            SessionEventKind::ServerToolResult { result, .. } => {
                close_slot(&mut slots, server_calls.get_mut(result.call_id()));
            }
            _ => {}
        }
    }

    SessionRepair {
        open: slots
            .into_iter()
            .flatten()
            .map(|slot| OpenRecord {
                outcome: turn_ends.outcome_for(slot.kind.turn(), slot.seq),
                seq: slot.seq,
                kind: slot.kind,
            })
            .collect(),
    }
}

struct OpenSlot {
    seq: u64,
    kind: OpenRecordKind,
}

/// Open one record and remember it under its correlation key.
///
/// `slots` keeps log order for free, so the projection is deterministic
/// without sorting hash-keyed state.
fn open_slot(
    slots: &mut Vec<Option<OpenSlot>>,
    queue: &mut VecDeque<usize>,
    seq: u64,
    kind: OpenRecordKind,
) {
    queue.push_back(slots.len());
    slots.push(Some(OpenSlot { seq, kind }));
}

/// Settle the oldest still-open record under one correlation key.
///
/// A closing event with nothing open — a `tool/result` for a call this slice
/// never saw requested — settles nothing and is not an error here; strict
/// correlation is [`crate::project_requests`]'s job.
fn close_slot(slots: &mut [Option<OpenSlot>], queue: Option<&mut VecDeque<usize>>) {
    if let Some(queue) = queue
        && let Some(index) = queue.pop_front()
        && let Some(slot) = slots.get_mut(index)
    {
        *slot = None;
    }
}

/// The latest `turn/end` recorded for each turn.
///
/// A turn that ended cannot deliver an outcome to anything still open inside
/// it, and that is the only conclusive evidence the envelope offers.
#[derive(Default)]
struct TurnEnds(BTreeMap<u64, u64>);

impl TurnEnds {
    fn record(&mut self, turn: u64, seq: u64) {
        self.0.insert(turn, seq);
    }

    fn outcome_for(&self, turn: u64, opened_at: u64) -> OpenOutcome {
        if self.0.get(&turn).is_some_and(|seq| *seq > opened_at) {
            OpenOutcome::Interrupted
        } else {
            OpenOutcome::Unknown
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{ToolCallOut, TurnEndReason};

    fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            v: crate::CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms: 1_730_000_000_000_i64 + seq as i64,
            kind,
        }
    }

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

    fn tool_result(id: &str) -> SessionEventKind {
        SessionEventKind::ToolResult {
            call_id: heycode_core::CallId::from_raw(id),
            content: "ok".to_owned(),
            is_error: false,
            untrusted_content: None,
        }
    }

    #[test]
    fn a_log_that_closed_everything_it_opened_has_nothing_to_repair() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, SessionEventKind::StepStart { turn: 0, step: 0 }),
            event(2, assistant_calling(0, &[("c1", "bash")])),
            event(3, dispatch(0, "c1", "bash")),
            event(4, tool_result("c1")),
            event(5, SessionEventKind::StepEnd { turn: 0, step: 0 }),
            event(
                6,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        let repair = project_repair(&events);
        assert!(repair.is_clean(), "{:?}", repair.open());
    }

    #[test]
    fn a_dispatched_call_the_log_never_answered_is_open_and_never_successful() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash")])),
            event(2, dispatch(0, "c1", "bash")),
        ];
        let repair = project_repair(&events);
        let calls = repair.unanswered_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].kind(),
            &OpenRecordKind::ToolCall {
                turn: 0,
                call_id: heycode_core::CallId::from_raw("c1"),
                name: "bash".to_owned(),
                dispatched: true,
            }
        );
        assert_eq!(calls[0].outcome(), OpenOutcome::Unknown);
    }

    #[test]
    fn a_call_declared_but_never_dispatched_is_reported_as_undispatched() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash")])),
        ];
        let repair = project_repair(&events);
        assert_eq!(
            repair.unanswered_tool_calls()[0].kind(),
            &OpenRecordKind::ToolCall {
                turn: 0,
                call_id: heycode_core::CallId::from_raw("c1"),
                name: "bash".to_owned(),
                dispatched: false,
            }
        );
    }

    #[test]
    fn a_later_successful_looking_event_never_settles_an_unanswered_call() {
        // The trap this row exists to prevent: the assistant message after the
        // call reads like the call worked. It answers nothing.
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash")])),
            event(2, dispatch(0, "c1", "bash")),
            event(
                3,
                SessionEventKind::AssistantMessage {
                    turn: 0,
                    step: 1,
                    content: "Done — the file is deleted.".to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                },
            ),
            event(
                4,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        let repair = project_repair(&events);
        assert_eq!(repair.unanswered_tool_calls().len(), 1);
        assert_eq!(
            repair.open()[0].outcome(),
            OpenOutcome::Interrupted,
            "its turn ended without it, so nothing can answer it now"
        );
    }

    #[test]
    fn a_result_settles_only_its_own_call_and_leaves_the_others_open() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash"), ("c2", "read")])),
            event(2, dispatch(0, "c1", "bash")),
            event(3, tool_result("c1")),
            event(4, dispatch(0, "c2", "read")),
        ];
        let repair = project_repair(&events);
        let open = repair.unanswered_tool_calls();
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].kind(),
            &OpenRecordKind::ToolCall {
                turn: 0,
                call_id: heycode_core::CallId::from_raw("c2"),
                name: "read".to_owned(),
                dispatched: true,
            }
        );
        assert_eq!(
            open[0].seq(),
            1,
            "the record opens where the model asked, not where the dispatch landed"
        );
    }

    #[test]
    fn a_result_cannot_settle_a_call_that_was_already_dispatched_alongside_it() {
        // Both calls are in flight when one answers. Settling by anything other
        // than the exact call id would retire the other one unanswered, which
        // is a dropped call wearing the shape of a finished one.
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash"), ("c2", "read")])),
            event(2, dispatch(0, "c1", "bash")),
            event(3, dispatch(0, "c2", "read")),
            event(4, tool_result("c1")),
        ];
        let open = project_repair(&events);
        let calls = open.unanswered_tool_calls();
        assert_eq!(calls.len(), 1, "c2 is still unanswered");
        assert_eq!(
            calls[0].kind(),
            &OpenRecordKind::ToolCall {
                turn: 0,
                call_id: heycode_core::CallId::from_raw("c2"),
                name: "read".to_owned(),
                dispatched: true,
            }
        );
        assert_eq!(calls[0].seq(), 1);
    }

    #[test]
    fn answering_the_second_call_first_still_leaves_only_the_first_open() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash"), ("c2", "read")])),
            event(2, dispatch(0, "c1", "bash")),
            event(3, dispatch(0, "c2", "read")),
            event(4, tool_result("c2")),
        ];
        let repair = project_repair(&events);
        let calls = repair.unanswered_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].kind(),
            &OpenRecordKind::ToolCall {
                turn: 0,
                call_id: heycode_core::CallId::from_raw("c1"),
                name: "bash".to_owned(),
                dispatched: true,
            },
            "settlement follows the call id, not the arrival order"
        );
    }

    #[test]
    fn a_later_turn_beginning_is_not_evidence_that_an_earlier_turn_ended() {
        // The envelope permits interleaved turns, so "turn 1 started" proves
        // nothing about turn 0. Calling turn 0 interrupted here would assert an
        // abandonment the log never recorded.
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, SessionEventKind::TurnStart { turn: 1 }),
        ];
        let repair = project_repair(&events);
        assert_eq!(
            repair
                .open()
                .iter()
                .map(|record| (record.kind().clone(), record.outcome()))
                .collect::<Vec<_>>(),
            vec![
                (OpenRecordKind::Turn { turn: 0 }, OpenOutcome::Unknown),
                (OpenRecordKind::Turn { turn: 1 }, OpenOutcome::Unknown),
            ]
        );
    }

    #[test]
    fn one_turn_ending_settles_only_records_inside_that_turn() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash")])),
            event(2, SessionEventKind::TurnStart { turn: 1 }),
            event(3, assistant_calling(1, &[("c2", "read")])),
            event(
                4,
                SessionEventKind::TurnEnd {
                    turn: 1,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        let repair = project_repair(&events);
        assert_eq!(
            repair
                .open()
                .iter()
                .map(|record| (record.kind().turn(), record.outcome()))
                .collect::<Vec<_>>(),
            vec![
                (0, OpenOutcome::Unknown),     // turn 0, still open
                (0, OpenOutcome::Unknown),     // its call: turn 0 never ended
                (1, OpenOutcome::Interrupted), // turn 1's call: turn 1 ended without it
            ],
            "turn 1 ending says nothing about turn 0's work"
        );
    }

    #[test]
    fn an_open_step_stays_unknown_while_its_own_turn_is_still_running() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, SessionEventKind::StepStart { turn: 0, step: 0 }),
            event(
                2,
                SessionEventKind::AssistantMessage {
                    turn: 0,
                    step: 0,
                    content: "thinking".to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                },
            ),
        ];
        let repair = project_repair(&events);
        assert_eq!(
            repair
                .open()
                .iter()
                .map(OpenRecord::outcome)
                .collect::<Vec<_>>(),
            vec![OpenOutcome::Unknown, OpenOutcome::Unknown],
            "the projection claims only what the log proves"
        );
    }

    #[test]
    fn a_step_left_open_by_a_turn_that_ended_without_it_is_interrupted() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, SessionEventKind::StepStart { turn: 0, step: 0 }),
            event(
                2,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Aborted,
                },
            ),
        ];
        let repair = project_repair(&events);
        assert_eq!(repair.open().len(), 1);
        assert_eq!(
            repair.open()[0].kind(),
            &OpenRecordKind::Step { turn: 0, step: 0 }
        );
        assert_eq!(repair.open()[0].outcome(), OpenOutcome::Interrupted);
    }

    #[test]
    fn an_unanswered_provider_executed_call_is_open_too() {
        let call = heycode_core::ServerToolCall::new(
            heycode_core::CallId::from_raw("srvtoolu_1"),
            "web_search",
            "web_search",
            serde_json::json!({"query": "rust"}),
        )
        .unwrap();
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(
                1,
                SessionEventKind::ServerToolCall {
                    turn: 0,
                    step: 2,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 0,
                    call: Box::new(call),
                },
            ),
        ];
        let repair = project_repair(&events);
        assert_eq!(
            repair.open()[1].kind(),
            &OpenRecordKind::ServerToolCall {
                turn: 0,
                step: 2,
                call_id: heycode_core::CallId::from_raw("srvtoolu_1"),
            }
        );
    }

    #[test]
    fn a_settled_provider_executed_call_is_not_reported() {
        let call = heycode_core::ServerToolCall::new(
            heycode_core::CallId::from_raw("srvtoolu_1"),
            "web_search",
            "web_search",
            serde_json::json!({"query": "rust"}),
        )
        .unwrap();
        let result = heycode_core::ServerToolResult::success(
            heycode_core::CallId::from_raw("srvtoolu_1"),
            Some(0),
            Vec::new(),
        )
        .unwrap();
        let events = vec![
            event(
                0,
                SessionEventKind::ServerToolCall {
                    turn: 0,
                    step: 2,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 0,
                    call: Box::new(call),
                },
            ),
            event(
                1,
                SessionEventKind::ServerToolResult {
                    turn: 0,
                    step: 2,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 1,
                    result: Box::new(result),
                },
            ),
        ];
        assert!(project_repair(&events).is_clean());
    }

    #[test]
    fn two_crashes_in_a_row_report_two_open_turns_and_do_not_compound() {
        // Crash inside turn 0, resume, crash again inside turn 1.
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash")])),
            event(2, dispatch(0, "c1", "bash")),
            event(3, SessionEventKind::TurnStart { turn: 1 }),
            event(4, assistant_calling(1, &[("c2", "read")])),
            event(5, dispatch(1, "c2", "read")),
        ];
        let repair = project_repair(&events);
        assert_eq!(
            repair.open().len(),
            4,
            "two open turns and two unanswered calls, counted once each"
        );
        assert!(
            repair
                .open()
                .iter()
                .all(|record| record.outcome() == OpenOutcome::Unknown),
            "no turn ever ended, so nothing here is provably abandoned"
        );
        assert_eq!(
            repair
                .open()
                .iter()
                .map(|record| record.kind().turn())
                .collect::<Vec<_>>(),
            vec![0, 0, 1, 1],
            "each crash contributes its own turn and its own call, once"
        );
        assert_eq!(
            project_repair(&events),
            repair,
            "the projection is pure: re-running it changes nothing"
        );
    }

    #[test]
    fn repairing_a_log_leaves_the_events_untouched_so_repair_is_idempotent() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, assistant_calling(0, &[("c1", "bash")])),
        ];
        let before = events.clone();
        let first = project_repair(&events);
        let second = project_repair(&events);
        assert_eq!(events, before, "the projection reads; it does not write");
        assert_eq!(first, second);
    }

    #[test]
    fn a_duplicate_start_leaves_the_extra_occurrence_open() {
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(1, SessionEventKind::TurnStart { turn: 0 }),
            event(
                2,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        let repair = project_repair(&events);
        assert_eq!(repair.open().len(), 1, "one end settles one start");
        assert_eq!(
            repair.open()[0].seq(),
            1,
            "the end settled the oldest occurrence, leaving the newer one open"
        );
        assert_eq!(repair.open()[0].outcome(), OpenOutcome::Interrupted);
    }

    #[test]
    fn a_reopened_turn_after_a_clean_close_is_unknown_not_interrupted() {
        // The earlier turn/end must not be read as evidence against a start
        // that came after it.
        let events = vec![
            event(0, SessionEventKind::TurnStart { turn: 0 }),
            event(
                1,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
            event(2, SessionEventKind::TurnStart { turn: 0 }),
        ];
        let repair = project_repair(&events);
        assert_eq!(repair.open().len(), 1);
        assert_eq!(repair.open()[0].seq(), 2);
        assert_eq!(repair.open()[0].outcome(), OpenOutcome::Unknown);
    }

    #[test]
    fn a_result_with_no_recorded_call_settles_nothing_and_invents_nothing() {
        let events = vec![event(0, tool_result("ghost"))];
        assert!(project_repair(&events).is_clean());
    }

    #[test]
    fn an_empty_log_has_nothing_open() {
        assert!(project_repair(&[]).is_clean());
        assert!(project_repair(&[]).unanswered_tool_calls().is_empty());
    }
}

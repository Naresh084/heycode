//! Shared R02-safe sequenced fan-out for delegated runtime adapters.
//!
//! Every delegated adapter has the same two obligations and used to discharge
//! them separately, wrongly and differently. This module owns both:
//!
//! 1. **Emissions are validated where they are produced.** Each `emit` goes
//!    through one session-lifetime [`RuntimeEventNormalizer`] before fan-out,
//!    so an adapter that publishes an R02-invalid sequence fails loud at its
//!    own call site instead of poisoning a distant consumer's stream.
//! 2. **Retention is bounded without breaking late subscribers.** Sequence
//!    numbers are per-subscription framing, not session identity: each
//!    subscriber is renumbered from zero, so the hub is free to evict old
//!    events. Eviction only ever removes whole settled turns, progress
//!    events, settled tool-call pairs and answered-or-not interaction
//!    requests, which keeps every retained window a valid R02 stream on its
//!    own.
//!
//! A long session therefore neither dies at the retention bound nor becomes
//! unsubscribable after it.

use std::collections::VecDeque;
use std::sync::Mutex;

use futures::channel::mpsc;
use futures::{StreamExt as _, stream};

use crate::{
    RuntimeError, RuntimeEvent, RuntimeEventKind, RuntimeEventNormalizer, RuntimeEventStream,
};

/// Events one hub retains for replay to a subscriber that attaches later.
pub const RUNTIME_EVENT_HISTORY: usize = 1_024;

struct HubState {
    next_sequence: u64,
    normalizer: RuntimeEventNormalizer,
    history: VecDeque<RuntimeEvent>,
    subscribers: Vec<mpsc::UnboundedSender<Result<RuntimeEvent, RuntimeError>>>,
    closed: bool,
    terminal: Option<RuntimeError>,
}

/// Sequenced, self-validating fan-out of one delegated session's events.
pub struct RuntimeEventHub {
    state: Mutex<HubState>,
    capacity: usize,
}

impl Default for RuntimeEventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeEventHub {
    /// A hub retaining [`RUNTIME_EVENT_HISTORY`] events for late subscribers.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(RUNTIME_EVENT_HISTORY)
    }

    /// A hub with an explicit retention bound.
    ///
    /// The bound is a floor of one event: session-ready is never evicted.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            state: Mutex::new(HubState {
                next_sequence: 0,
                normalizer: RuntimeEventNormalizer::new(),
                history: VecDeque::new(),
                subscribers: Vec::new(),
                closed: false,
                terminal: None,
            }),
            capacity: capacity.max(1),
        }
    }

    /// Sequence, validate, retain and fan out one adapter event.
    ///
    /// # Errors
    /// A payload, ordering, correlation or settlement violation is reported as
    /// `Protocol` at the emitting call site and permanently seals the hub's
    /// validator. Emitting into a failed or closed hub fails the same way.
    pub fn emit(&self, kind: RuntimeEventKind) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::internal("runtime event hub"))?;
        if state.terminal.is_some() {
            return Err(RuntimeError::protocol());
        }
        if state.closed {
            return Err(RuntimeError::closed());
        }
        let event = RuntimeEvent::new(state.next_sequence, kind);
        let event = state
            .normalizer
            .push(event)
            .map_err(|_| RuntimeError::protocol())?
            .into_event();
        state.next_sequence = state
            .next_sequence
            .checked_add(1)
            .ok_or_else(RuntimeError::protocol)?;
        state.history.push_back(event.clone());
        trim(&mut state.history, self.capacity);
        state
            .subscribers
            .retain_mut(|sender| sender.unbounded_send(Ok(event.clone())).is_ok());
        Ok(())
    }

    /// Settle every subscriber with one terminal error.
    ///
    /// The first failure wins; later calls and later emissions are ignored.
    pub fn fail(&self, error: RuntimeError) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        state.closed = true;
        state.terminal = Some(error.clone());
        for sender in state.subscribers.drain(..) {
            let _sent = sender.unbounded_send(Err(error.clone()));
            sender.close_channel();
        }
    }

    /// End every subscriber cleanly after a quiescent close.
    pub fn close(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        state.closed = true;
        for sender in state.subscribers.drain(..) {
            sender.close_channel();
        }
    }

    /// A stream replaying the retained window and then live events.
    ///
    /// Sequences are minted per subscription, so the returned stream always
    /// starts at sequence zero with session-ready and stays contiguous.
    #[must_use]
    pub fn subscribe(&self) -> RuntimeEventStream {
        let (sender, receiver) = mpsc::unbounded();
        let Ok(mut state) = self.state.lock() else {
            return resequence(Box::pin(stream::once(async {
                Err(RuntimeError::internal("runtime event hub"))
            })));
        };
        let replay = state.history.iter().cloned().collect::<Vec<_>>();
        let head = stream::iter(replay.into_iter().map(Ok));
        let source: RuntimeEventStream = if state.closed {
            match state.terminal.clone() {
                Some(error) => Box::pin(head.chain(stream::once(async move { Err(error) }))),
                None => Box::pin(head),
            }
        } else {
            state.subscribers.push(sender);
            Box::pin(head.chain(receiver))
        };
        drop(state);
        resequence(source)
    }
}

/// Renumber one subscription from zero so a trimmed window stays contiguous.
fn resequence(source: RuntimeEventStream) -> RuntimeEventStream {
    let mut next = 0_u64;
    Box::pin(source.map(move |item| {
        let event = item?;
        let sequence = next;
        next = next.checked_add(1).ok_or_else(RuntimeError::protocol)?;
        Ok(RuntimeEvent::new(sequence, event.kind().clone()))
    }))
}

/// Shrink the retained window to `capacity` without invalidating it.
///
/// Eviction always takes the oldest event it can, finest grain first: a
/// progress event, then a settled tool-call pair, then an interaction request,
/// and only once a settled turn has nothing left to shave does the turn's
/// skeleton go as one block. Anything a retained later event still depends on
/// — session-ready, an unsettled turn's skeleton, an open tool call — is never
/// removed, so a window that cannot shrink further is kept whole rather than
/// corrupted.
fn trim(history: &mut VecDeque<RuntimeEvent>, capacity: usize) {
    while history.len() > capacity {
        if drop_oldest(history, is_progress)
            || drop_oldest_settled_tool_call(history)
            || drop_oldest(history, is_interaction)
            || drop_oldest_settled_turn(history)
        {
            continue;
        }
        return;
    }
}

/// Remove the earliest turn that already settled, skeleton included.
fn drop_oldest_settled_turn(history: &mut VecDeque<RuntimeEvent>) -> bool {
    let Some((start, turn)) =
        history
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event.kind() {
                RuntimeEventKind::TurnStarted { turn } => Some((index, turn.clone())),
                _ => None,
            })
    else {
        return false;
    };
    // Turns never overlap, so an unsettled turn is always the last one and no
    // earlier block can be removed.
    let Some(end) = history
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, event)| match event.kind() {
            RuntimeEventKind::TurnFinished { turn: settled, .. } if *settled == turn => Some(index),
            _ => None,
        })
    else {
        return false;
    };
    let mut kept = history.split_off(end + 1);
    history.truncate(start);
    history.append(&mut kept);
    true
}

/// Remove the earliest event nothing later depends on.
fn drop_oldest(
    history: &mut VecDeque<RuntimeEvent>,
    evictable: fn(&RuntimeEventKind) -> bool,
) -> bool {
    let Some(index) = history.iter().position(|event| evictable(event.kind())) else {
        return false;
    };
    let _dropped = history.remove(index);
    true
}

/// Remove the earliest tool call that already reported its result.
fn drop_oldest_settled_tool_call(history: &mut VecDeque<RuntimeEvent>) -> bool {
    for index in 0..history.len() {
        let Some(RuntimeEventKind::ToolCall { call_id, .. }) =
            history.get(index).map(RuntimeEvent::kind)
        else {
            continue;
        };
        let call_id = call_id.clone();
        let settled = history
            .iter()
            .enumerate()
            .skip(index)
            .find_map(|(position, event)| match event.kind() {
                RuntimeEventKind::ToolResult {
                    call_id: settled, ..
                } if *settled == call_id => Some(position),
                _ => None,
            });
        let Some(settled) = settled else {
            continue;
        };
        let _result = history.remove(settled);
        let _call = history.remove(index);
        return true;
    }
    false
}

/// Text, accounting and notice phases carry no correlation obligation.
const fn is_progress(kind: &RuntimeEventKind) -> bool {
    matches!(
        kind,
        RuntimeEventKind::CommentaryDelta { .. }
            | RuntimeEventKind::ReasoningDelta { .. }
            | RuntimeEventKind::Usage { .. }
            | RuntimeEventKind::Notice { .. }
    )
}

/// Permission/question requests are answered out of band, never in-stream.
const fn is_interaction(kind: &RuntimeEventKind) -> bool {
    matches!(
        kind,
        RuntimeEventKind::PermissionRequested { .. } | RuntimeEventKind::QuestionRequested { .. }
    )
}

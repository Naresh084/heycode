//! A03 — durable follow-up/steer/inject admission and deterministic wake rules.
//!
//! C06 owns the durable `agent/inbox/splice` vocabulary; this module owns the
//! live agent behavior on top of it. Three questions have exactly one answer
//! each, and all three are decided from lifecycle state rather than inferred:
//!
//! * **Where does an input queue?** [`InboxDelivery::target`] — follow-up waits
//!   for a new turn, steer and inject wait for the next step boundary.
//! * **Does submitting wake an idle agent?** Follow-up and steer wake; inject
//!   never does. A busy agent is never woken, because the running turn already
//!   drains both queues.
//! * **When is queued text model-visible?** Only when a claim atomically
//!   removes it and appends the matching `user/message`.

use heycode_session::{
    InboxDelivery, InboxMessage, InboxMessageId, InboxSource, InboxTarget, SessionEventKind,
};

use crate::agent::Agent;
use crate::ui::UiEvent;

/// What the caller must do after a durable inbox submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxWake {
    /// The message is queued and an existing owner will claim it. The caller
    /// must not start a turn: a running turn drains both queues, and a queued
    /// follow-up is drained when that turn settles.
    Queued,
    /// The agent was idle and this delivery wakes it. The caller owns starting
    /// exactly one turn through [`Agent::send_follow_up_cancellable`] (next
    /// turn) or by resuming the current one (next step).
    Wake,
}

/// Counts of currently pending inbox work, by queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InboxPending {
    /// Inputs waiting for a new turn.
    pub next_turn: usize,
    /// Inputs waiting for the next step boundary.
    pub next_step: usize,
}

pub(crate) struct InboxSubmission {
    id: InboxMessageId,
    pending: InboxPending,
    wake: InboxWake,
}

impl InboxSubmission {
    pub(crate) const fn id(&self) -> &InboxMessageId {
        &self.id
    }
}

impl InboxPending {
    /// Whether any queue holds work.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.next_turn == 0 && self.next_step == 0
    }
}

/// Why a follow-up turn could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUpError {
    /// No input is waiting for a new turn.
    Empty,
}

impl std::fmt::Display for FollowUpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("no follow-up input is pending"),
        }
    }
}

impl std::error::Error for FollowUpError {}

impl Agent {
    /// Currently pending inbox work.
    #[must_use]
    pub fn pending_inbox(&self) -> InboxPending {
        let session = self.session().lock().unwrap_or_else(|e| e.into_inner());
        let inbox = session.inbox();
        InboxPending {
            next_turn: inbox.next_turn().len(),
            next_step: inbox.next_step().len(),
        }
    }

    /// Pending human text, in admission order and scoped to this agent.
    #[must_use]
    pub fn pending_human_messages(&self) -> Vec<InboxMessage> {
        self.session()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_human_messages()
    }

    /// Recall only human messages that have not yet been consumed.
    ///
    /// # Errors
    /// Durable recall failed; no text should be restored by the caller.
    pub fn recall_human_messages(&self) -> anyhow::Result<Vec<InboxMessage>> {
        let (messages, pending) = {
            let mut session = self.session().lock().unwrap_or_else(|e| e.into_inner());
            let messages = session.recall_human_messages()?;
            (messages, pending_from(&session))
        };
        self.emit_inbox(pending, InboxWake::Queued);
        Ok(messages)
    }

    /// Select a wakeable pending occurrence after a turn settles.
    /// Inject-only context never starts an otherwise idle agent.
    #[must_use]
    pub fn next_wakeable_message(&self) -> Option<InboxMessageId> {
        let session = self.session().lock().unwrap_or_else(|e| e.into_inner());
        session
            .inbox()
            .next_turn()
            .first()
            .or_else(|| {
                session.inbox().next_step().first().filter(|_| {
                    session
                        .inbox()
                        .next_step()
                        .iter()
                        .any(|m| m.delivery() == InboxDelivery::Steer)
                })
            })
            .map(|message| message.id().clone())
    }

    /// Insert a team delivery once across pending and already-claimed history.
    /// The durable inbox id is supplied by the trusted team host, never the model.
    pub(crate) fn deliver_team_mail(&self, id: &str, text: &str) -> anyhow::Result<bool> {
        let id = InboxMessageId::new(id)?;
        let pending = {
            let mut session = self
                .session()
                .lock()
                .map_err(|_| anyhow::anyhow!("team inbox unavailable"))?;
            if session.events().iter().any(|event| matches!(&event.kind, SessionEventKind::AgentInboxSplice { inserted, .. } if inserted.iter().any(|message| message.id() == &id))) {
                return Ok(false);
            }
            let message =
                InboxMessage::with_source(id, InboxDelivery::FollowUp, text, InboxSource::Team)?;
            let start = u32::try_from(session.inbox().next_turn().len())?;
            session.append(SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start,
                removed_count: None,
                inserted: vec![message],
                outcome: None,
            })?;
            session.flush()?;
            pending_from(&session)
        };
        self.emit_inbox(
            pending,
            if self.token().is_turn_active() {
                InboxWake::Queued
            } else {
                InboxWake::Wake
            },
        );
        Ok(true)
    }

    /// Durably queue one operational input and report whether the caller owes a
    /// wake.
    ///
    /// The message is appended to the end of its delivery's queue. Wake is
    /// decided only after the durable append succeeds, from the authoritative
    /// turn-liveness state, so a reported wake can never describe a submission
    /// that rolled back.
    ///
    /// # Errors
    /// Blank or oversized text, a duplicate occurrence id, or durable append
    /// failure. Nothing is queued and no UI event publishes on failure.
    pub fn submit_inbox(
        &self,
        delivery: InboxDelivery,
        text: impl Into<String>,
    ) -> anyhow::Result<(InboxMessageId, InboxWake)> {
        self.submit_inbox_with_source(delivery, text, InboxSource::Human)
    }

    pub(crate) fn submit_inbox_with_source(
        &self,
        delivery: InboxDelivery,
        text: impl Into<String>,
        source: InboxSource,
    ) -> anyhow::Result<(InboxMessageId, InboxWake)> {
        let submission = self.enqueue_inbox_with_source(delivery, text, source)?;
        let id = submission.id.clone();
        let wake = submission.wake;
        self.publish_inbox_submission(submission);
        Ok((id, wake))
    }

    pub(crate) fn enqueue_inbox_with_source(
        &self,
        delivery: InboxDelivery,
        text: impl Into<String>,
        source: InboxSource,
    ) -> anyhow::Result<InboxSubmission> {
        enqueue_owned_inbox(
            self.session(),
            self.token().is_turn_active(),
            delivery,
            text.into(),
            source,
        )
    }

    pub(crate) fn publish_inbox_submission(&self, submission: InboxSubmission) {
        self.emit_inbox(submission.pending, submission.wake);
    }

    /// Durably cancel one pending input that has not been claimed.
    ///
    /// # Errors
    /// Durable append failure. Returns `Ok(false)` when the id is unknown or
    /// already settled, which is not an error: cancellation races admission.
    pub fn cancel_inbox(&self, id: &InboxMessageId) -> anyhow::Result<bool> {
        let pending = {
            let mut session = self.session().lock().unwrap_or_else(|e| e.into_inner());
            let Some((target, start)) = locate(&session, id) else {
                return Ok(false);
            };
            session.append(SessionEventKind::AgentInboxSplice {
                target,
                start,
                removed_count: Some(1),
                inserted: Vec::new(),
                outcome: Some(heycode_session::InboxSpliceOutcome::Canceled),
            })?;
            pending_from(&session)
        };
        self.emit_inbox(pending, InboxWake::Queued);
        Ok(true)
    }

    /// Claim every input currently waiting at a step boundary and admit each as
    /// model-visible text, oldest first.
    ///
    /// Each claim is one atomic splice/`user/message` pair, so an interrupted
    /// drain leaves a consistent prefix claimed and the remainder pending.
    ///
    /// # Errors
    /// Durable append failure.
    pub(crate) fn drain_next_step(&self) -> anyhow::Result<Vec<String>> {
        let mut admitted = Vec::new();
        loop {
            let claimed = {
                let mut session = self.session().lock().unwrap_or_else(|e| e.into_inner());
                if session.inbox().next_step().is_empty() {
                    None
                } else {
                    Some(session.append_inbox_claim(InboxTarget::NextStep, 0)?)
                }
            };
            let Some(message) = claimed else { break };
            let text = message.text().to_owned();
            self.publish_admission(&text);
            admitted.push(text);
        }
        if !admitted.is_empty() {
            let pending = self.pending_inbox();
            self.emit_inbox(pending, InboxWake::Queued);
        }
        Ok(admitted)
    }

    /// Claim exactly the oldest input waiting for a new turn and admit it.
    ///
    /// One queued input opens each new turn; the rest stay pending so their
    /// order and count survive resume.
    ///
    /// # Errors
    /// Durable append failure.
    pub(crate) fn claim_follow_up(
        &self,
        expected: &InboxMessageId,
    ) -> anyhow::Result<Option<String>> {
        let claimed = {
            let mut session = self.session().lock().unwrap_or_else(|e| e.into_inner());
            if session.inbox().next_turn().is_empty() {
                None
            } else {
                if session.inbox().next_turn()[0].id() != expected {
                    anyhow::bail!("follow-up changed while lifecycle hooks were running");
                }
                Some(session.append_inbox_claim(InboxTarget::NextTurn, 0)?)
            }
        };
        let Some(message) = claimed else {
            return Ok(None);
        };
        let text = message.text().to_owned();
        self.publish_admission(&text);
        let pending = self.pending_inbox();
        self.emit_inbox(pending, InboxWake::Queued);
        Ok(Some(text))
    }

    pub(crate) fn pending_inbox_text(&self, id: &InboxMessageId) -> anyhow::Result<Option<String>> {
        let session = self.session().lock().unwrap_or_else(|e| e.into_inner());
        let Some((target, index)) = locate(&session, id) else {
            return Ok(None);
        };
        if index != 0 {
            anyhow::bail!("selected pending input is no longer first in its queue");
        }
        let messages = match target {
            InboxTarget::NextTurn => session.inbox().next_turn(),
            InboxTarget::NextStep => session.inbox().next_step(),
        };
        Ok(messages.first().map(|message| message.text().to_owned()))
    }

    pub(crate) fn claim_inbox_id(&self, id: &InboxMessageId) -> anyhow::Result<Option<String>> {
        let message = {
            let mut session = self.session().lock().unwrap_or_else(|e| e.into_inner());
            let Some((target, index)) = locate(&session, id) else {
                return Ok(None);
            };
            if index != 0 {
                anyhow::bail!("pending input changed while lifecycle hooks were running");
            }
            session.append_inbox_claim(target, index)?
        };
        let text = message.text().to_owned();
        self.publish_admission(&text);
        self.emit_inbox(self.pending_inbox(), InboxWake::Queued);
        Ok(Some(text))
    }

    /// Announce settlement-time inbox state so an owner can start the follow-up
    /// turn a busy submission deliberately did not wake.
    pub(crate) fn announce_settled_inbox(&self) {
        let pending = self.pending_inbox();
        let wake = if self.next_wakeable_message().is_some() {
            InboxWake::Wake
        } else {
            InboxWake::Queued
        };
        self.emit_inbox(pending, wake);
    }

    fn publish_admission(&self, text: &str) {
        self.emit_ui(UiEvent::UserEcho {
            text: text.to_owned(),
        });
    }

    fn emit_inbox(&self, pending: InboxPending, wake: InboxWake) {
        self.emit_ui(UiEvent::InboxUpdated {
            next_turn: pending.next_turn,
            next_step: pending.next_step,
            wake,
        });
        if wake == InboxWake::Wake {
            self.request_inbox_wake();
        }
    }
}

fn queue_len(session: &heycode_session::Session, target: InboxTarget) -> usize {
    match target {
        InboxTarget::NextTurn => session.inbox().next_turn().len(),
        InboxTarget::NextStep => session.inbox().next_step().len(),
    }
}

fn pending_from(session: &heycode_session::Session) -> InboxPending {
    InboxPending {
        next_turn: session.inbox().next_turn().len(),
        next_step: session.inbox().next_step().len(),
    }
}

fn locate(session: &heycode_session::Session, id: &InboxMessageId) -> Option<(InboxTarget, u32)> {
    let inbox = session.inbox();
    let find = |messages: &[InboxMessage]| {
        messages
            .iter()
            .position(|message| message.id() == id)
            .and_then(|index| u32::try_from(index).ok())
    };
    find(inbox.next_turn())
        .map(|start| (InboxTarget::NextTurn, start))
        .or_else(|| find(inbox.next_step()).map(|start| (InboxTarget::NextStep, start)))
}

/// Shared durable enqueue for a captured caller owner across a background boundary.
pub(crate) fn enqueue_owned_inbox(
    session: &std::sync::Mutex<heycode_session::Session>,
    active: bool,
    delivery: InboxDelivery,
    text: String,
    source: InboxSource,
) -> anyhow::Result<InboxSubmission> {
    let message = InboxMessage::with_source(InboxMessageId::generate(), delivery, text, source)?;
    let id = message.id().clone();
    let target = delivery.target();
    let pending = {
        let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
        let start = u32::try_from(queue_len(&session, target))
            .map_err(|_| anyhow::anyhow!("inbox queue exceeds the supported range"))?;
        session.append(SessionEventKind::AgentInboxSplice {
            target,
            start,
            removed_count: None,
            inserted: vec![message],
            outcome: None,
        })?;
        pending_from(&session)
    };
    // Durable first, then publish. A busy agent is never woken: its turn
    // drains next-step work at every boundary and re-drains before it
    // settles, and a queued follow-up is announced at that settlement.
    let wake = if active {
        InboxWake::Queued
    } else {
        match delivery {
            InboxDelivery::FollowUp | InboxDelivery::Steer => InboxWake::Wake,
            InboxDelivery::Inject => InboxWake::Queued,
        }
    };
    Ok(InboxSubmission { id, pending, wake })
}

/// Admit a trusted durable occurrence once, including across claims and retries.
/// A retry flushes an earlier in-memory append before reporting success.
pub(crate) fn enqueue_identified_inbox(
    session: &std::sync::Mutex<heycode_session::Session>,
    active: bool,
    message: InboxMessage,
) -> anyhow::Result<InboxSubmission> {
    let id = message.id().clone();
    let delivery = message.delivery();
    let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
    let existing = session.events().iter().find_map(|event| match &event.kind {
        SessionEventKind::AgentInboxSplice { inserted, .. } => {
            inserted.iter().find(|entry| entry.id() == &id)
        }
        _ => None,
    });
    if let Some(existing) = existing {
        anyhow::ensure!(
            existing == &message,
            "inbox occurrence identity conflicts with its durable payload"
        );
    } else {
        let start = u32::try_from(queue_len(&session, delivery.target()))?;
        session.append(SessionEventKind::AgentInboxSplice {
            target: delivery.target(),
            start,
            removed_count: None,
            inserted: vec![message],
            outcome: None,
        })?;
    }
    session.flush()?;
    let is_pending = locate(&session, &id).is_some();
    let pending = pending_from(&session);
    let wake = if is_pending && !active && delivery != InboxDelivery::Inject {
        InboxWake::Wake
    } else {
        InboxWake::Queued
    };
    Ok(InboxSubmission { id, pending, wake })
}
pub(crate) fn publish_owned_inbox(bus: &heycode_core::EventBus, submission: InboxSubmission) {
    bus.emit(UiEvent::InboxUpdated {
        next_turn: submission.pending.next_turn,
        next_step: submission.pending.next_step,
        wake: submission.wake,
    });
}

//! Durable pending-input vocabulary and replay projection.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{CURRENT_SESSION_LOG_VERSION, SessionEvent, SessionEventKind};

const MAX_INBOX_ID_BYTES: usize = 128;
const MAX_INBOX_TEXT_BYTES: usize = 1024 * 1024;

/// Validation failure for one durable inbox message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InboxMessageError {
    /// The occurrence identity is blank, unsafe or too large.
    #[error("invalid inbox message id")]
    InvalidId,
    /// The model-visible text is blank or too large.
    #[error("invalid inbox message text")]
    InvalidText,
}

/// Stable occurrence identity for one queued input.
///
/// An id may be inserted only once in one durable session. Replacements mint
/// a new id so every claim/cancellation settlement remains unambiguous.
/// `Deserialize` deliberately does **not** re-validate here, unlike
/// [`ScreenedText`](heycode_live_artifact) and telemetry's `Label`. Every path
/// that deserializes an id runs it through the inbox projection, which rejects
/// an invalid one as `OpenError::InvalidEvent` naming the log line. Validating
/// inside serde instead would classify it as `CorruptLine`, and the query path
/// treats that as a possibly-transient partial write and **retries** — turning
/// a permanent error into three futile attempts. The guard belongs at the layer
/// that can classify it (GOTCHAS #161 names the general rule and this
/// exception).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InboxMessageId(String);

impl InboxMessageId {
    /// Construct one validated externally supplied occurrence id.
    ///
    /// # Errors
    /// The id is blank, exceeds 128 bytes or contains characters outside
    /// ASCII alphanumerics plus `-`, `_`, `.`, and `:`.
    pub fn new(value: impl Into<String>) -> Result<Self, InboxMessageError> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    /// Mint one fresh occurrence id.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Borrow the opaque id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), InboxMessageError> {
        if self.0.is_empty()
            || self.0.len() > MAX_INBOX_ID_BYTES
            || !self.0.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            Err(InboxMessageError::InvalidId)
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for InboxMessageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How one input entered the agent inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxDelivery {
    /// Start a later turn, waking an idle agent.
    FollowUp,
    /// Enter at the next step, waking an idle agent.
    Steer,
    /// Enter at the next step without waking an idle agent.
    Inject,
}

/// Terminal result of one attributed agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCompletionOutcome {
    /// The run completed successfully.
    Completed,
    /// The run failed.
    Failed,
    /// The owner cancelled the run.
    Cancelled,
    /// The runtime stopped before a terminal result was available.
    Interrupted,
}

/// Durable attribution for one operational input before model admission.
///
/// Human input is the backward-compatible default and is omitted from the
/// event payload. Automation sources retain only validated opaque identities
/// and counters; the exact text remains on [`InboxMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxSource {
    /// Registry-authenticated agent input. Names describe the sender at admission;
    /// they never grant human authority or determine routing.
    Agent {
        /// Stable sender conversation identity.
        agent_id: String,
        /// Event-time sender display name.
        agent_name: String,
        /// Stable recipient conversation identity.
        recipient_id: String,
        /// Distinct sender invocation identity.
        run_id: String,
        /// Stable terminal occurrence; absent for ordinary messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completion_id: Option<String>,
        /// Terminal result; present exactly when completion_id is present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<AgentCompletionOutcome>,
    },
    /// Automatically delivered team peer input. The envelope text retains sender
    /// identity and is ordinary untrusted conversation input.
    Team,

    /// Ordinary human/control-plane input.
    #[default]
    Human,
    /// Human answer to a durable optional question. Text retains the exact model envelope.
    OptionalQuestion {
        /// Same durable identity as the admitted answer message.
        question_id: InboxMessageId,
        /// Explicit selected labels; absent for legacy and custom text answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_answers: Option<Vec<String>>,
    },
    /// Settlement notice from an effect-owned background job.
    Job {
        /// Stable job identity.
        job_id: String,
    },
    /// One same-session goal continuation round.
    Goal {
        /// Goal identity at reservation.
        goal_id: crate::GoalId,
        /// Exact goal revision at reservation.
        revision: u64,
        /// Positive sequential goal round.
        round: u32,
    },
    /// One durable session-local schedule occurrence.
    Schedule {
        /// Schedule identity in the owning session suffix.
        schedule_id: crate::ScheduleId,
        /// Exact due occurrence represented by this input.
        occurrence_at_ms: i64,
    },
}

impl InboxSource {
    fn is_human(&self) -> bool {
        matches!(self, Self::Human)
    }

    fn validate(&self) -> Result<(), InboxMessageError> {
        match self {
            Self::Agent {
                agent_id,
                agent_name,
                recipient_id,
                run_id,
                completion_id,
                outcome,
            } => {
                for id in [agent_id, recipient_id, run_id] {
                    InboxMessageId::new(id.clone())?;
                }
                if let Some(id) = completion_id {
                    InboxMessageId::new(id.clone())?;
                }
                if completion_id.is_some() != outcome.is_some()
                    || agent_name.trim().is_empty()
                    || agent_name.len() > 256
                    || agent_name.chars().any(char::is_control)
                {
                    return Err(InboxMessageError::InvalidText);
                }
                Ok(())
            }
            Self::Human | Self::Team => Ok(()),
            Self::OptionalQuestion {
                question_id,
                selected_answers,
            } => {
                if let Some(labels) = selected_answers
                    && (labels.is_empty()
                        || labels.len() > 4
                        || labels.iter().any(|label| {
                            label.trim().is_empty()
                                || label.len() > 256
                                || label.chars().any(char::is_control)
                        })
                        || labels
                            .iter()
                            .collect::<std::collections::HashSet<_>>()
                            .len()
                            != labels.len())
                {
                    return Err(InboxMessageError::InvalidText);
                }
                InboxMessageId::new(question_id.as_str()).map(|_| ())
            }
            Self::Job { job_id } => {
                let valid = job_id
                    .strip_prefix("job-")
                    .and_then(|number| number.parse::<u64>().ok())
                    .is_some_and(|number| format!("job-{number}") == *job_id);
                if valid {
                    Ok(())
                } else {
                    Err(InboxMessageError::InvalidId)
                }
            }
            Self::Goal {
                goal_id,
                revision,
                round,
            } => {
                if crate::GoalId::new(goal_id.as_str()).is_ok() && *revision > 0 && *round > 0 {
                    Ok(())
                } else {
                    Err(InboxMessageError::InvalidId)
                }
            }
            Self::Schedule {
                schedule_id,
                occurrence_at_ms,
            } => {
                if crate::ScheduleId::new(schedule_id.as_str()).is_ok() && *occurrence_at_ms >= 0 {
                    Ok(())
                } else {
                    Err(InboxMessageError::InvalidId)
                }
            }
        }
    }
}

impl InboxDelivery {
    /// Required pending list for this delivery behavior.
    #[must_use]
    pub const fn target(self) -> InboxTarget {
        match self {
            Self::FollowUp => InboxTarget::NextTurn,
            Self::Steer | Self::Inject => InboxTarget::NextStep,
        }
    }
}

/// One of the two ordered pending-input lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxTarget {
    /// One queued input is admitted by each new turn.
    NextTurn,
    /// All currently due input is admitted at the next step boundary.
    NextStep,
}

impl fmt::Display for InboxTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NextTurn => formatter.write_str("next_turn"),
            Self::NextStep => formatter.write_str("next_step"),
        }
    }
}

/// One exact model-visible input pending admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxMessage {
    id: InboxMessageId,
    delivery: InboxDelivery,
    text: String,
    #[serde(default, skip_serializing_if = "InboxSource::is_human")]
    source: InboxSource,
}

impl InboxMessage {
    /// Construct a message with a fresh occurrence id.
    ///
    /// # Errors
    /// The text is blank or exceeds one MiB.
    pub fn new(
        delivery: InboxDelivery,
        text: impl Into<String>,
    ) -> Result<Self, InboxMessageError> {
        Self::with_id(InboxMessageId::generate(), delivery, text)
    }

    /// Construct a message with an externally supplied occurrence id.
    ///
    /// # Errors
    /// The id or text violates the durable inbox boundary.
    pub fn with_id(
        id: InboxMessageId,
        delivery: InboxDelivery,
        text: impl Into<String>,
    ) -> Result<Self, InboxMessageError> {
        let message = Self {
            id,
            delivery,
            text: text.into(),
            source: InboxSource::Human,
        };
        message.validate()?;
        Ok(message)
    }

    /// Construct a message with explicit durable automation attribution.
    ///
    /// # Errors
    /// The identity, text, or source violates the durable inbox boundary.
    pub fn with_source(
        id: InboxMessageId,
        delivery: InboxDelivery,
        text: impl Into<String>,
        source: InboxSource,
    ) -> Result<Self, InboxMessageError> {
        let message = Self {
            id,
            delivery,
            text: text.into(),
            source,
        };
        message.validate()?;
        Ok(message)
    }

    /// Stable occurrence identity.
    #[must_use]
    pub fn id(&self) -> &InboxMessageId {
        &self.id
    }

    /// Admission and wake behavior selected by the caller.
    #[must_use]
    pub const fn delivery(&self) -> InboxDelivery {
        self.delivery
    }

    /// Exact model-visible text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Durable source attribution.
    #[must_use]
    pub const fn source(&self) -> &InboxSource {
        &self.source
    }

    fn validate(&self) -> Result<(), InboxMessageError> {
        self.id.validate()?;
        self.source.validate()?;
        if let InboxSource::OptionalQuestion { question_id, .. } = &self.source
            && question_id != &self.id
        {
            return Err(InboxMessageError::InvalidId);
        }
        if let InboxSource::OptionalQuestion {
            selected_answers: Some(labels),
            ..
        } = &self.source
        {
            let suffix =
                serde_json::to_string(labels).map_err(|_| InboxMessageError::InvalidText)?;
            if !self.text.ends_with(&format!("\n\n{suffix}")) {
                return Err(InboxMessageError::InvalidText);
            }
        }
        if self.text.trim().is_empty() || self.text.len() > MAX_INBOX_TEXT_BYTES {
            Err(InboxMessageError::InvalidText)
        } else {
            Ok(())
        }
    }
}

/// Explicit reason a splice removed pending input without admitting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxSpliceOutcome {
    /// The removed work will not run.
    Canceled,
}

/// One removed message and the durable event that settled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxSettlement {
    seq: u64,
    target: InboxTarget,
    message: InboxMessage,
}

impl InboxSettlement {
    /// Sequence of the settling splice.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Pending list from which the message was removed.
    #[must_use]
    pub const fn target(&self) -> InboxTarget {
        self.target
    }

    /// Removed occurrence.
    #[must_use]
    pub const fn message(&self) -> &InboxMessage {
        &self.message
    }
}

/// Replayed state and settlement accounting for all inbox splice events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxProjection {
    next_turn: Vec<InboxMessage>,
    next_step: Vec<InboxMessage>,
    claimed: Vec<InboxSettlement>,
    canceled: Vec<InboxSettlement>,
    seen_ids: std::collections::HashSet<InboxMessageId>,
}

pub(crate) struct ValidatedInboxSplice {
    target: InboxTarget,
    start: usize,
    removed_count: usize,
    outcome: Option<InboxSpliceOutcome>,
}

impl InboxProjection {
    /// Inputs waiting for individual future turns.
    #[must_use]
    pub fn next_turn(&self) -> &[InboxMessage] {
        &self.next_turn
    }

    /// Inputs waiting for the next step boundary.
    #[must_use]
    pub fn next_step(&self) -> &[InboxMessage] {
        &self.next_step
    }

    /// Inputs removed by claim splices, in durable order.
    #[must_use]
    pub fn claimed(&self) -> &[InboxSettlement] {
        &self.claimed
    }

    /// Inputs removed as canceled work, in durable order.
    #[must_use]
    pub fn canceled(&self) -> &[InboxSettlement] {
        &self.canceled
    }

    pub(crate) fn apply_event(&mut self, event: &SessionEvent) -> Result<(), InboxProjectionError> {
        if let Some(validated) = self.validate_event(event)? {
            self.commit_validated_event(event, validated);
        }
        Ok(())
    }

    pub(crate) fn validate_event(
        &self,
        event: &SessionEvent,
    ) -> Result<Option<ValidatedInboxSplice>, InboxProjectionError> {
        let SessionEventKind::AgentInboxSplice {
            target,
            start,
            removed_count,
            inserted,
            outcome,
        } = &event.kind
        else {
            return Ok(None);
        };
        if event.v != CURRENT_SESSION_LOG_VERSION {
            return Err(InboxProjectionError::UnsupportedEventVersion {
                seq: event.seq,
                found: event.v,
            });
        }
        validate_splice_shape(*target, *removed_count, inserted, *outcome).map_err(|message| {
            InboxProjectionError::InvalidSplice {
                seq: event.seq,
                message,
            }
        })?;
        for message in inserted {
            if self.seen_ids.contains(message.id()) {
                return Err(InboxProjectionError::ReusedMessageId {
                    seq: event.seq,
                    id: message.id().clone(),
                });
            }
        }

        let start_u32 = *start;
        let start = usize::try_from(start_u32).map_err(|_| InboxProjectionError::OutOfBounds {
            seq: event.seq,
            target: *target,
            start: start_u32,
            removed_count: removed_count.unwrap_or(0),
            pending_len: self.queue(*target).len(),
        })?;
        let removed_count_u32 = removed_count.unwrap_or(0);
        let removed_count =
            usize::try_from(removed_count_u32).map_err(|_| InboxProjectionError::OutOfBounds {
                seq: event.seq,
                target: *target,
                start: start_u32,
                removed_count: removed_count_u32,
                pending_len: self.queue(*target).len(),
            })?;
        let pending_len = self.queue(*target).len();
        if start > pending_len || removed_count > pending_len.saturating_sub(start) {
            return Err(InboxProjectionError::OutOfBounds {
                seq: event.seq,
                target: *target,
                start: start_u32,
                removed_count: removed_count_u32,
                pending_len,
            });
        }

        Ok(Some(ValidatedInboxSplice {
            target: *target,
            start,
            removed_count,
            outcome: *outcome,
        }))
    }

    pub(crate) fn commit_validated_event(
        &mut self,
        event: &SessionEvent,
        validated: ValidatedInboxSplice,
    ) {
        let SessionEventKind::AgentInboxSplice { inserted, .. } = &event.kind else {
            return;
        };
        let queue = match validated.target {
            InboxTarget::NextTurn => &mut self.next_turn,
            InboxTarget::NextStep => &mut self.next_step,
        };
        let removed = queue
            .splice(
                validated.start..validated.start + validated.removed_count,
                inserted.iter().cloned(),
            )
            .collect::<Vec<_>>();
        self.seen_ids
            .extend(inserted.iter().map(|message| message.id().clone()));
        let settlements = removed.into_iter().map(|message| InboxSettlement {
            seq: event.seq,
            target: validated.target,
            message,
        });
        match validated.outcome {
            Some(InboxSpliceOutcome::Canceled) => self.canceled.extend(settlements),
            None => self.claimed.extend(settlements),
        }
    }

    fn queue(&self, target: InboxTarget) -> &[InboxMessage] {
        match target {
            InboxTarget::NextTurn => &self.next_turn,
            InboxTarget::NextStep => &self.next_step,
        }
    }
}

/// Structural failure while rebuilding the durable inbox.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InboxProjectionError {
    /// This projection understands inbox events only in the current v2 envelope.
    #[error("inbox splice at seq {seq} requires session envelope v2, found v{found}")]
    UnsupportedEventVersion {
        /// Event sequence.
        seq: u64,
        /// Source envelope version.
        found: u8,
    },
    /// One splice violates the normalized shape contract.
    #[error("invalid inbox splice at seq {seq}: {message}")]
    InvalidSplice {
        /// Event sequence.
        seq: u64,
        /// Safe validation detail.
        message: String,
    },
    /// Coordinates do not fit the pre-splice target list.
    #[error(
        "inbox splice at seq {seq} is out of bounds for {target}: start {start}, remove {removed_count}, pending {pending_len}"
    )]
    OutOfBounds {
        /// Event sequence.
        seq: u64,
        /// Mutated list.
        target: InboxTarget,
        /// Requested normalized position.
        start: u32,
        /// Requested normalized removal count.
        removed_count: u32,
        /// Pre-splice pending length.
        pending_len: usize,
    },
    /// One occurrence id was inserted by an earlier durable splice.
    #[error("inbox splice at seq {seq} reuses message id `{id}`")]
    ReusedMessageId {
        /// Event sequence.
        seq: u64,
        /// Reused occurrence id.
        id: InboxMessageId,
    },
}

/// Fold all normalized inbox splices into pending state and settlement history.
///
/// Compaction does not shadow these operational events: an input remains
/// pending until a later durable claim or cancellation removes it.
///
/// # Errors
/// A splice has an invalid shape, incompatible message/target, duplicate
/// durable identity, unsupported source version or out-of-bounds coordinates.
pub fn project_inbox(events: &[SessionEvent]) -> Result<InboxProjection, InboxProjectionError> {
    let mut projection = InboxProjection::default();
    for event in events {
        projection.apply_event(event)?;
    }
    Ok(projection)
}

pub(crate) fn validate_splice_shape(
    target: InboxTarget,
    removed_count: Option<u32>,
    inserted: &[InboxMessage],
    outcome: Option<InboxSpliceOutcome>,
) -> Result<(), String> {
    if removed_count == Some(0) {
        return Err("removed_count must be absent when zero".to_owned());
    }
    if removed_count.is_none() && inserted.is_empty() {
        return Err("splice must insert or remove at least one message".to_owned());
    }
    if outcome.is_some() && removed_count.is_none() {
        return Err("canceled outcome requires removed messages".to_owned());
    }
    if removed_count.is_some() && outcome.is_none() && !inserted.is_empty() {
        return Err("a claim must be a pure deletion".to_owned());
    }
    let mut inserted_ids = std::collections::HashSet::new();
    for message in inserted {
        message.validate().map_err(|error| error.to_string())?;
        if message.delivery().target() != target {
            return Err("message delivery does not match splice target".to_owned());
        }
        if !inserted_ids.insert(message.id()) {
            return Err("inserted message ids must be unique".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod optional_question_source_tests {
    use super::*;

    #[test]
    fn optional_answer_source_round_trips_and_retains_exact_text() {
        let id = InboxMessageId::new("question-1").unwrap();
        let text = "Answer to optional question [question-1]: Format?\n\nMarkdown";
        let message = InboxMessage::with_source(
            id.clone(),
            InboxDelivery::FollowUp,
            text,
            InboxSource::OptionalQuestion {
                selected_answers: None,
                question_id: id.clone(),
            },
        )
        .unwrap();
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["source"]["kind"], "optional_question");
        let recovered: InboxMessage = serde_json::from_value(json).unwrap();
        recovered.validate().unwrap();
        assert_eq!(recovered.text(), text);
        assert_eq!(recovered.id(), &id);
        assert_eq!(recovered.source(), message.source());
    }

    #[test]
    fn legacy_human_payload_remains_human_and_mismatched_question_id_is_refused() {
        let message: InboxMessage = serde_json::from_value(serde_json::json!({"id":"question-1","delivery":"follow_up","text":"Answer to optional question [question-1]: user-authored text"})).unwrap();
        message.validate().unwrap();
        assert_eq!(message.source(), &InboxSource::Human);
        let mismatched = InboxMessage::with_source(
            InboxMessageId::new("question-1").unwrap(),
            InboxDelivery::FollowUp,
            "answer",
            InboxSource::OptionalQuestion {
                selected_answers: None,
                question_id: InboxMessageId::new("question-2").unwrap(),
            },
        );
        assert!(mismatched.is_err());
    }
    #[test]
    fn selected_optional_labels_require_matching_durable_answer_body() {
        let id = InboxMessageId::new("question-1").unwrap();
        let source = InboxSource::OptionalQuestion {
            question_id: id.clone(),
            selected_answers: Some(vec!["A".into(), "B".into()]),
        };
        let message = InboxMessage::with_source(
            id.clone(),
            InboxDelivery::FollowUp,
            "Answer to optional question [question-1]: Scope?\n\n[\"A\",\"B\"]",
            source.clone(),
        )
        .unwrap();
        let recovered: InboxMessage =
            serde_json::from_value(serde_json::to_value(&message).unwrap()).unwrap();
        recovered.validate().unwrap();
        assert_eq!(recovered.source(), &source);
        assert!(
            InboxMessage::with_source(
                id,
                InboxDelivery::FollowUp,
                "Answer to optional question [question-1]: Scope?\n\ncustom text",
                source
            )
            .is_err()
        );
    }
}

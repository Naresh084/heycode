//! Strict validation boundary for delegated-runtime event streams.

use std::collections::HashSet;
use std::fmt::{Debug, Formatter};
use std::io::{Error as IoError, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use heycode_core::CallId;

use crate::{
    RuntimeError, RuntimeEvent, RuntimeEventKind, RuntimeEventStream, RuntimeFinishReason,
    RuntimeRequestId, RuntimeTurnId,
};

const MAX_DELTA_BYTES: usize = 256 * 1024;
const MAX_FINAL_BYTES: usize = 1024 * 1024;
const MAX_STRUCTURED_BYTES: usize = 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;
const MAX_ACTION_BYTES: usize = 256;
const MAX_DETAIL_BYTES: usize = 16 * 1024;
const MAX_CHOICE_BYTES: usize = 512;
const MAX_CHOICES: usize = 32;
const MAX_NOTICE_CODE_BYTES: usize = 64;
const MAX_NOTICE_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_CALL_ID_BYTES: usize = 256;
const MAX_TOOL_NAME_BYTES: usize = 128;

/// Maximum retained unique ids in each turn, tool-call and interaction plane.
///
/// Providers should rotate a delegated bridge session before this boundary.
/// The cap keeps exact duplicate detection deterministic without allowing an
/// unbounded long-lived stream to grow validator memory indefinitely.
pub const MAX_RUNTIME_EVENT_IDENTITIES: usize = 65_536;

/// Stable, content-free classification of an invalid runtime event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEventViolationCode {
    /// Sequence did not equal the next contiguous value, beginning at zero.
    SequenceGap,
    /// A provider exhausted the sequence-number space.
    SequenceExhausted,
    /// Sequence zero was not the one required session-ready event.
    SessionReadyRequired,
    /// Session-ready appeared more than once.
    DuplicateSessionReady,
    /// A turn-scoped phase arrived without an active turn.
    TurnRequired,
    /// A new turn began before the active turn settled.
    TurnAlreadyActive,
    /// A previously settled provider turn id was reused.
    DuplicateTurn,
    /// Turn settlement did not correlate with the active turn.
    TurnMismatch,
    /// A tool call id was reused.
    DuplicateToolCall,
    /// A tool result did not correlate with a known call.
    UnknownToolCall,
    /// A tool call settled more than once.
    DuplicateToolResult,
    /// A turn or final message arrived while tool calls remained open.
    UnsettledToolCall,
    /// A permission/question request id was reused.
    DuplicateRequest,
    /// Exact correlation history reached its per-plane memory bound.
    CorrelationCapacityExceeded,
    /// A turn published more than one complete final message.
    DuplicateFinalMessage,
    /// A non-usage/non-settlement phase arrived after the final message.
    PhaseAfterFinal,
    /// A normally stopped turn omitted its complete final message.
    MissingFinalMessage,
    /// A payload violated a size, shape or safe-text boundary.
    InvalidPayload,
    /// A finite replay ended before session/turn settlement.
    IncompleteReplay,
    /// A caller attempted to continue a failed normalizer.
    NormalizerFailed,
    /// A caller attempted to append after finishing a replay.
    NormalizerFinished,
}

impl RuntimeEventViolationCode {
    const fn message(self) -> &'static str {
        match self {
            Self::SequenceGap => "runtime event sequence is not contiguous",
            Self::SequenceExhausted => "runtime event sequence space is exhausted",
            Self::SessionReadyRequired => "runtime replay must begin with session ready",
            Self::DuplicateSessionReady => "runtime session ready appeared more than once",
            Self::TurnRequired => "runtime event requires an active turn",
            Self::TurnAlreadyActive => "runtime turns overlap",
            Self::DuplicateTurn => "runtime turn id was reused",
            Self::TurnMismatch => "runtime turn settlement does not match the active turn",
            Self::DuplicateToolCall => "runtime tool call id was reused",
            Self::UnknownToolCall => "runtime tool result has no matching call",
            Self::DuplicateToolResult => "runtime tool call settled more than once",
            Self::UnsettledToolCall => "runtime turn has an unsettled tool call",
            Self::DuplicateRequest => "runtime interaction request id was reused",
            Self::CorrelationCapacityExceeded => "runtime event correlation capacity is exhausted",
            Self::DuplicateFinalMessage => "runtime turn has multiple final messages",
            Self::PhaseAfterFinal => "runtime phase arrived after the final message",
            Self::MissingFinalMessage => "runtime stopped without a final message",
            Self::InvalidPayload => "runtime event payload is invalid",
            Self::IncompleteReplay => "runtime event replay ended before settlement",
            Self::NormalizerFailed => "runtime event normalizer has failed",
            Self::NormalizerFinished => "runtime event normalizer is already finished",
        }
    }
}

/// Redacted runtime-event validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", .code.message())]
pub struct RuntimeEventViolation {
    code: RuntimeEventViolationCode,
}

impl RuntimeEventViolation {
    const fn new(code: RuntimeEventViolationCode) -> Self {
        Self { code }
    }

    /// Stable failure classification.
    #[must_use]
    pub const fn code(self) -> RuntimeEventViolationCode {
        self.code
    }
}

/// An event that passed R02 sequence, payload, correlation and settlement checks.
#[derive(Clone, PartialEq)]
pub struct NormalizedRuntimeEvent(RuntimeEvent);

impl NormalizedRuntimeEvent {
    /// Provider-session-local contiguous sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.0.sequence()
    }

    /// Validated provider-neutral semantic payload.
    #[must_use]
    pub const fn kind(&self) -> &RuntimeEventKind {
        self.0.kind()
    }

    /// Recover the validated raw event for a projection owner.
    #[must_use]
    pub fn into_event(self) -> RuntimeEvent {
        self.0
    }
}

impl Debug for NormalizedRuntimeEvent {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NormalizedRuntimeEvent")
            .field("sequence", &self.sequence())
            .field("phase", &runtime_event_phase(self.kind()))
            .finish()
    }
}

struct ActiveTurn {
    id: RuntimeTurnId,
    final_seen: bool,
    open_calls: HashSet<CallId>,
}

impl ActiveTurn {
    fn new(id: RuntimeTurnId) -> Self {
        Self {
            id,
            final_seen: false,
            open_calls: HashSet::new(),
        }
    }
}

/// Incremental R02 validator for one complete provider-session replay.
///
/// A normalizer is single-use. The first violation poisons it, and `finish`
/// seals it after proving that no turn or tool call remains active. `Usage` is
/// accounting for an inner provider/model step, may repeat within one runtime
/// turn and is not a terminal phase; only `FinalMessage` and `TurnFinished`
/// constrain later turn phases.
pub struct RuntimeEventNormalizer {
    next_sequence: u64,
    ready: bool,
    active: Option<ActiveTurn>,
    seen_turns: HashSet<RuntimeTurnId>,
    seen_calls: HashSet<CallId>,
    seen_requests: HashSet<RuntimeRequestId>,
    failed: bool,
    finished: bool,
}

impl RuntimeEventNormalizer {
    /// Create a validator expecting sequence zero and session-ready.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_sequence: 0,
            ready: false,
            active: None,
            seen_turns: HashSet::new(),
            seen_calls: HashSet::new(),
            seen_requests: HashSet::new(),
            failed: false,
            finished: false,
        }
    }

    /// Validate and commit one raw provider event.
    ///
    /// The returned wrapper is the only event type intended for durable/UI
    /// projection. Payload values are never included in validation errors.
    ///
    /// # Errors
    /// Returns a stable redacted violation and permanently fails this
    /// normalizer when ordering, correlation, settlement or payload is invalid.
    pub fn push(
        &mut self,
        event: RuntimeEvent,
    ) -> Result<NormalizedRuntimeEvent, RuntimeEventViolation> {
        if self.failed {
            return Err(RuntimeEventViolation::new(
                RuntimeEventViolationCode::NormalizerFailed,
            ));
        }
        if self.finished {
            return Err(RuntimeEventViolation::new(
                RuntimeEventViolationCode::NormalizerFinished,
            ));
        }

        let result = self.validate_and_commit(&event);
        if let Err(code) = result {
            self.failed = true;
            return Err(RuntimeEventViolation::new(code));
        }
        Ok(NormalizedRuntimeEvent(event))
    }

    /// Seal a finite replay after proving it ends between turns.
    ///
    /// # Errors
    /// Empty/unready replays and active/unsettled turns fail closed.
    pub fn finish(&mut self) -> Result<(), RuntimeEventViolation> {
        if self.failed {
            return Err(RuntimeEventViolation::new(
                RuntimeEventViolationCode::NormalizerFailed,
            ));
        }
        if self.finished {
            return Ok(());
        }
        if !self.ready || self.active.is_some() {
            self.failed = true;
            return Err(RuntimeEventViolation::new(
                RuntimeEventViolationCode::IncompleteReplay,
            ));
        }
        self.finished = true;
        Ok(())
    }

    fn validate_and_commit(
        &mut self,
        event: &RuntimeEvent,
    ) -> Result<(), RuntimeEventViolationCode> {
        validate_payload(event.kind())?;
        if event.sequence() != self.next_sequence {
            return Err(RuntimeEventViolationCode::SequenceGap);
        }
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(RuntimeEventViolationCode::SequenceExhausted)?;

        if !self.ready {
            if !matches!(event.kind(), RuntimeEventKind::SessionReady) {
                return Err(RuntimeEventViolationCode::SessionReadyRequired);
            }
            self.ready = true;
            self.next_sequence = next_sequence;
            return Ok(());
        }

        match event.kind() {
            RuntimeEventKind::SessionReady => {
                return Err(RuntimeEventViolationCode::DuplicateSessionReady);
            }
            RuntimeEventKind::TurnStarted { turn } => {
                if self.active.is_some() {
                    return Err(RuntimeEventViolationCode::TurnAlreadyActive);
                }
                if self.seen_turns.contains(turn) {
                    return Err(RuntimeEventViolationCode::DuplicateTurn);
                }
                ensure_identity_capacity(self.seen_turns.len())?;
                self.seen_turns.insert(turn.clone());
                self.active = Some(ActiveTurn::new(turn.clone()));
            }
            RuntimeEventKind::CommentaryDelta { .. } | RuntimeEventKind::ReasoningDelta { .. } => {
                self.check_open_phase()?;
            }
            RuntimeEventKind::PermissionRequested { request_id, .. }
            | RuntimeEventKind::QuestionRequested { request_id, .. } => {
                self.check_open_phase()?;
                if self.seen_requests.contains(request_id) {
                    return Err(RuntimeEventViolationCode::DuplicateRequest);
                }
                ensure_identity_capacity(self.seen_requests.len())?;
                self.seen_requests.insert(request_id.clone());
            }
            RuntimeEventKind::ToolCall { call_id, .. } => {
                self.check_open_phase()?;
                if self.seen_calls.contains(call_id) {
                    return Err(RuntimeEventViolationCode::DuplicateToolCall);
                }
                ensure_identity_capacity(self.seen_calls.len())?;
                self.seen_calls.insert(call_id.clone());
                let active = self
                    .active
                    .as_mut()
                    .ok_or(RuntimeEventViolationCode::TurnRequired)?;
                active.open_calls.insert(call_id.clone());
            }
            RuntimeEventKind::ToolResult { call_id, .. } => {
                self.check_open_phase()?;
                if !self.seen_calls.contains(call_id) {
                    return Err(RuntimeEventViolationCode::UnknownToolCall);
                }
                let active = self
                    .active
                    .as_mut()
                    .ok_or(RuntimeEventViolationCode::TurnRequired)?;
                if !active.open_calls.remove(call_id) {
                    return Err(RuntimeEventViolationCode::DuplicateToolResult);
                }
            }
            RuntimeEventKind::FinalMessage { .. } => {
                let active = self
                    .active
                    .as_mut()
                    .ok_or(RuntimeEventViolationCode::TurnRequired)?;
                if active.final_seen {
                    return Err(RuntimeEventViolationCode::DuplicateFinalMessage);
                }
                if !active.open_calls.is_empty() {
                    return Err(RuntimeEventViolationCode::UnsettledToolCall);
                }
                active.final_seen = true;
            }
            RuntimeEventKind::ContextBudgetChanged { .. } | RuntimeEventKind::Usage { .. } => {
                self.active
                    .as_ref()
                    .ok_or(RuntimeEventViolationCode::TurnRequired)?;
            }
            RuntimeEventKind::TurnFinished { turn, reason } => {
                let active = self
                    .active
                    .as_ref()
                    .ok_or(RuntimeEventViolationCode::TurnRequired)?;
                if &active.id != turn {
                    return Err(RuntimeEventViolationCode::TurnMismatch);
                }
                if !active.open_calls.is_empty() {
                    return Err(RuntimeEventViolationCode::UnsettledToolCall);
                }
                if *reason == RuntimeFinishReason::Stop && !active.final_seen {
                    return Err(RuntimeEventViolationCode::MissingFinalMessage);
                }
                self.active = None;
            }
            RuntimeEventKind::Notice { .. } => {}
        }
        self.next_sequence = next_sequence;
        Ok(())
    }

    fn check_open_phase(&self) -> Result<(), RuntimeEventViolationCode> {
        let active = self
            .active
            .as_ref()
            .ok_or(RuntimeEventViolationCode::TurnRequired)?;
        if active.final_seen {
            return Err(RuntimeEventViolationCode::PhaseAfterFinal);
        }
        Ok(())
    }
}

impl Default for RuntimeEventNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Stream of events that passed the delegated-runtime normalization boundary.
pub type NormalizedRuntimeEventStream =
    Pin<Box<dyn Stream<Item = Result<NormalizedRuntimeEvent, RuntimeError>> + Send + 'static>>;

/// Validate a live raw runtime subscription before durable/UI projection.
///
/// The wrapper emits each valid event unchanged. The first violation emits one
/// fixed `Protocol` error and terminates; an upstream error is preserved and
/// never followed by a synthetic finish. Unsolicited live EOF is a protocol
/// failure even between turns; a quiescent close owner drops its subscriber.
#[must_use]
pub fn normalize_runtime_event_stream(source: RuntimeEventStream) -> NormalizedRuntimeEventStream {
    Box::pin(NormalizingStream {
        source,
        normalizer: RuntimeEventNormalizer::new(),
        done: false,
        clean_eof: false,
    })
}

/// Validate one finite replay snapshot and accept settled clean EOF.
///
/// Use `normalize_runtime_event_stream` for a live subscription: unsolicited
/// live EOF is a protocol failure. A replay must still begin at sequence zero
/// with session-ready and end between turns.
#[must_use]
pub fn normalize_runtime_event_replay(source: RuntimeEventStream) -> NormalizedRuntimeEventStream {
    Box::pin(NormalizingStream {
        source,
        normalizer: RuntimeEventNormalizer::new(),
        done: false,
        clean_eof: true,
    })
}

struct NormalizingStream {
    source: RuntimeEventStream,
    normalizer: RuntimeEventNormalizer,
    done: bool,
    clean_eof: bool,
}

impl Stream for NormalizingStream {
    type Item = Result<NormalizedRuntimeEvent, RuntimeError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.source.as_mut().poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => match self.normalizer.push(event) {
                Ok(event) => Poll::Ready(Some(Ok(event))),
                Err(_) => {
                    self.done = true;
                    Poll::Ready(Some(Err(RuntimeError::protocol())))
                }
            },
            Poll::Ready(Some(Err(error))) => {
                self.done = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.done = true;
                let settled = self.normalizer.finish().is_ok();
                if settled && self.clean_eof {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(RuntimeError::protocol())))
                }
            }
        }
    }
}

fn ensure_identity_capacity(current: usize) -> Result<(), RuntimeEventViolationCode> {
    if current >= MAX_RUNTIME_EVENT_IDENTITIES {
        return Err(RuntimeEventViolationCode::CorrelationCapacityExceeded);
    }
    Ok(())
}

fn runtime_event_phase(kind: &RuntimeEventKind) -> &'static str {
    match kind {
        RuntimeEventKind::SessionReady => "session_ready",
        RuntimeEventKind::TurnStarted { .. } => "turn_started",
        RuntimeEventKind::CommentaryDelta { .. } => "commentary_delta",
        RuntimeEventKind::ReasoningDelta { .. } => "reasoning_delta",
        RuntimeEventKind::FinalMessage { .. } => "final_message",
        RuntimeEventKind::ToolCall { .. } => "tool_call",
        RuntimeEventKind::ToolResult { .. } => "tool_result",
        RuntimeEventKind::PermissionRequested { .. } => "permission_requested",
        RuntimeEventKind::QuestionRequested { .. } => "question_requested",
        RuntimeEventKind::ContextBudgetChanged { .. } => "context_budget_changed",
        RuntimeEventKind::Usage { .. } => "usage",
        RuntimeEventKind::TurnFinished { .. } => "turn_finished",
        RuntimeEventKind::Notice { .. } => "notice",
    }
}

fn validate_payload(kind: &RuntimeEventKind) -> Result<(), RuntimeEventViolationCode> {
    match kind {
        RuntimeEventKind::SessionReady
        | RuntimeEventKind::TurnStarted { .. }
        | RuntimeEventKind::TurnFinished { .. } => Ok(()),
        RuntimeEventKind::ContextBudgetChanged { budget } => {
            if budget.window == Some(0)
                || budget.model.is_empty()
                || budget.model.len() > 256
                || budget.model.chars().any(char::is_control)
            {
                return Err(RuntimeEventViolationCode::InvalidPayload);
            }
            Ok(())
        }
        RuntimeEventKind::Usage { context, .. } => {
            if context
                .as_ref()
                .is_some_and(|context| context.context_window == 0)
            {
                return Err(RuntimeEventViolationCode::InvalidPayload);
            }
            if let Some(model) = context
                .as_ref()
                .and_then(|context| context.resolved_model.as_deref())
                && (model.is_empty()
                    || model.len() > 256
                    || model.trim() != model
                    || model.chars().any(char::is_control))
            {
                return Err(RuntimeEventViolationCode::InvalidPayload);
            }
            Ok(())
        }
        RuntimeEventKind::CommentaryDelta { text } => {
            validate_output_text(text, MAX_DELTA_BYTES, false)
        }
        RuntimeEventKind::ReasoningDelta { text } => {
            validate_output_text(text, MAX_DELTA_BYTES, true)
        }
        RuntimeEventKind::FinalMessage { text } => {
            validate_output_text(text, MAX_FINAL_BYTES, true)
        }
        RuntimeEventKind::ToolCall {
            call_id,
            name,
            arguments,
        } => {
            validate_call_id(call_id)?;
            validate_one_line(name, MAX_TOOL_NAME_BYTES)?;
            if !arguments.is_object() {
                return Err(RuntimeEventViolationCode::InvalidPayload);
            }
            validate_json(arguments)
        }
        RuntimeEventKind::ToolResult {
            call_id, result, ..
        } => {
            validate_call_id(call_id)?;
            validate_json(result)
        }
        RuntimeEventKind::PermissionRequested { action, detail, .. } => {
            validate_one_line(action, MAX_ACTION_BYTES)?;
            validate_safe_block(detail, MAX_DETAIL_BYTES)
        }
        RuntimeEventKind::QuestionRequested {
            mode,
            progress,
            header,
            prompt,
            choices,
            choice_descriptions,
            ..
        } => {
            if progress.0 == 0
                || progress.0 > progress.1
                || progress.1 > 16
                || (*mode == heycode_core::QuestionMode::FreeText && !choices.is_empty())
                || (*mode != heycode_core::QuestionMode::FreeText && choices.is_empty())
            {
                return Err(RuntimeEventViolationCode::InvalidPayload);
            }
            if let Some(header) = header {
                validate_one_line(header, MAX_ACTION_BYTES)?;
            }
            validate_safe_block(prompt, MAX_DETAIL_BYTES)?;
            if choices.len() > MAX_CHOICES || choice_descriptions.len() != choices.len() {
                return Err(RuntimeEventViolationCode::InvalidPayload);
            }
            let mut unique = HashSet::with_capacity(choices.len());
            for (choice, description) in choices.iter().zip(choice_descriptions) {
                validate_one_line(choice, MAX_CHOICE_BYTES)?;
                if let Some(description) = description {
                    validate_safe_block(description, MAX_DETAIL_BYTES)?;
                }
                if !unique.insert(choice.as_str()) {
                    return Err(RuntimeEventViolationCode::InvalidPayload);
                }
            }
            Ok(())
        }
        RuntimeEventKind::Notice { code, message } => {
            validate_notice_code(code)?;
            validate_safe_block(message, MAX_NOTICE_MESSAGE_BYTES)
        }
    }
}

fn validate_output_text(
    text: &str,
    limit: usize,
    allow_empty: bool,
) -> Result<(), RuntimeEventViolationCode> {
    if (!allow_empty && text.is_empty())
        || text.len() > limit
        || text.chars().any(is_unsafe_terminal_control)
    {
        return Err(RuntimeEventViolationCode::InvalidPayload);
    }
    Ok(())
}

fn validate_safe_block(text: &str, limit: usize) -> Result<(), RuntimeEventViolationCode> {
    if text.trim() != text {
        return Err(RuntimeEventViolationCode::InvalidPayload);
    }
    validate_output_text(text, limit, false)
}

fn validate_one_line(text: &str, limit: usize) -> Result<(), RuntimeEventViolationCode> {
    if text.is_empty()
        || text.trim() != text
        || text.len() > limit
        || text.chars().any(char::is_control)
    {
        return Err(RuntimeEventViolationCode::InvalidPayload);
    }
    Ok(())
}

fn is_unsafe_terminal_control(character: char) -> bool {
    character.is_control() && character != '\n' && character != '\t'
}

fn validate_call_id(call_id: &CallId) -> Result<(), RuntimeEventViolationCode> {
    validate_one_line(call_id.as_str(), MAX_CALL_ID_BYTES)
}

fn validate_notice_code(code: &str) -> Result<(), RuntimeEventViolationCode> {
    let bytes = code.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_NOTICE_CODE_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(*byte, b'.' | b'_' | b'-')
        });
    if !valid {
        return Err(RuntimeEventViolationCode::InvalidPayload);
    }
    Ok(())
}

fn validate_json(value: &serde_json::Value) -> Result<(), RuntimeEventViolationCode> {
    let mut stack = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes
            .checked_add(1)
            .ok_or(RuntimeEventViolationCode::InvalidPayload)?;
        if nodes > MAX_JSON_NODES || depth > MAX_JSON_DEPTH {
            return Err(RuntimeEventViolationCode::InvalidPayload);
        }
        match value {
            serde_json::Value::Array(values) => {
                ensure_json_frontier(nodes, stack.len(), values.len())?;
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                ensure_json_frontier(nodes, stack.len(), values.len())?;
                if values
                    .keys()
                    .any(|key| key.chars().any(is_unsafe_terminal_control))
                {
                    return Err(RuntimeEventViolationCode::InvalidPayload);
                }
                stack.extend(values.values().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
            serde_json::Value::String(value) => {
                if value.chars().any(is_unsafe_terminal_control) {
                    return Err(RuntimeEventViolationCode::InvalidPayload);
                }
            }
        }
    }

    let mut writer = CappedWriter::new(MAX_STRUCTURED_BYTES);
    serde_json::to_writer(&mut writer, value).map_err(|_| RuntimeEventViolationCode::InvalidPayload)
}

fn ensure_json_frontier(
    visited: usize,
    pending: usize,
    incoming: usize,
) -> Result<(), RuntimeEventViolationCode> {
    let retained = visited
        .checked_add(pending)
        .and_then(|count| count.checked_add(incoming))
        .ok_or(RuntimeEventViolationCode::InvalidPayload)?;
    if retained > MAX_JSON_NODES {
        return Err(RuntimeEventViolationCode::InvalidPayload);
    }
    Ok(())
}

struct CappedWriter {
    written: usize,
    limit: usize,
}

impl CappedWriter {
    const fn new(limit: usize) -> Self {
        Self { written: 0, limit }
    }
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next = self
            .written
            .checked_add(buffer.len())
            .ok_or_else(|| IoError::new(ErrorKind::FileTooLarge, "runtime JSON is too large"))?;
        if next > self.limit {
            return Err(IoError::new(
                ErrorKind::FileTooLarge,
                "runtime JSON is too large",
            ));
        }
        self.written = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

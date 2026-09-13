//! Stable provider failure classification and replay-safe retry execution.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    InferenceEvent, InferenceStream, LlmError, ProviderErrorClass, ProviderErrorCode,
    ProviderFailure, ProviderFailureOrigin,
};

const MAX_RETRY_ATTEMPTS: u8 = 8;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
static JITTER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Whether ambiguous transport failures may replay one stateless request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrySafety {
    /// Disable automatic replay.
    Never,
    /// Retry only definitive HTTP/provider failures.
    DefinitiveFailuresOnly,
    /// The complete request is stateless and replayable until output starts.
    StatelessPreOutput,
}

/// Backoff jitter policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryJitter {
    /// Exact exponential delay.
    None,
    /// Uniform deterministic sample in `0..=backoff` supplied by the caller.
    Full,
}

/// Invalid retry metadata.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RetrySpecError {
    /// Field relationship or hard bound is invalid.
    #[error("invalid retry field `{field}`: {message}")]
    Invalid {
        /// Stable field name.
        field: &'static str,
        /// Safe fixed detail.
        message: &'static str,
    },
}

/// Explicit retry policy resolved into a one-shot call before dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrySpec {
    max_attempts: u8,
    base_delay: Duration,
    max_backoff: Duration,
    max_retry_after: Duration,
    jitter: RetryJitter,
    safety: RetrySafety,
}

impl RetrySpec {
    /// Validate one bounded policy. `max_attempts` includes the initial call.
    ///
    /// # Errors
    /// Attempts must be 1..=8, base ≤ backoff ≤ 60 seconds and Retry-After
    /// bound ≤ five minutes.
    pub fn new(
        max_attempts: u8,
        base_delay: Duration,
        max_backoff: Duration,
        max_retry_after: Duration,
        jitter: RetryJitter,
        safety: RetrySafety,
    ) -> Result<Self, RetrySpecError> {
        if !(1..=MAX_RETRY_ATTEMPTS).contains(&max_attempts) {
            return Err(invalid_retry("max_attempts", "attempts must be in 1..=8"));
        }
        if base_delay > max_backoff || max_backoff > MAX_BACKOFF {
            return Err(invalid_retry(
                "backoff",
                "base must not exceed a maximum of 60 seconds",
            ));
        }
        if !base_delay.subsec_nanos().is_multiple_of(1_000_000)
            || !max_backoff.subsec_nanos().is_multiple_of(1_000_000)
            || !max_retry_after.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(invalid_retry(
                "duration",
                "retry durations must use whole milliseconds",
            ));
        }
        if max_retry_after > MAX_RETRY_AFTER {
            return Err(invalid_retry(
                "max_retry_after",
                "Retry-After bound must not exceed five minutes",
            ));
        }
        Ok(Self {
            max_attempts,
            base_delay,
            max_backoff,
            max_retry_after,
            jitter,
            safety,
        })
    }

    /// Official-SDK-shaped default: initial request plus two bounded retries.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
            max_retry_after: Duration::from_secs(60),
            jitter: RetryJitter::Full,
            safety: RetrySafety::StatelessPreOutput,
        }
    }

    /// Disable automatic replay explicitly.
    #[must_use]
    pub fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            base_delay: Duration::ZERO,
            max_backoff: Duration::ZERO,
            max_retry_after: Duration::ZERO,
            jitter: RetryJitter::None,
            safety: RetrySafety::Never,
        }
    }

    /// Attempts this policy allows, including the first.
    #[must_use]
    pub const fn max_attempts(&self) -> u8 {
        self.max_attempts
    }

    /// Effective replay-safety proof.
    #[must_use]
    pub const fn safety(&self) -> RetrySafety {
        self.safety
    }

    /// Replace replay-safety evidence without changing timing/attempt bounds.
    #[must_use]
    pub const fn with_safety(mut self, safety: RetrySafety) -> Self {
        self.safety = safety;
        self
    }

    pub(crate) fn disable_replay(mut self) -> Self {
        self.safety = RetrySafety::Never;
        self
    }

    /// Decide the next action from body-free failure facts and exact attempt
    /// state.
    #[must_use]
    pub fn decide(&self, error: &LlmError, attempt: RetryAttempt) -> RetryDecision {
        if attempt.output_emitted {
            return RetryDecision::DoNotRetry(RetryStopReason::OutputAlreadyEmitted);
        }
        let Some(failure) = error.provider_failure() else {
            return RetryDecision::DoNotRetry(RetryStopReason::NonRetryableClass);
        };
        if failure.class() == ProviderErrorClass::Cancelled {
            return RetryDecision::DoNotRetry(RetryStopReason::Cancelled);
        }
        if self.safety == RetrySafety::Never || self.max_attempts <= 1 {
            return RetryDecision::DoNotRetry(RetryStopReason::Disabled);
        }
        if attempt.number >= self.max_attempts {
            return RetryDecision::DoNotRetry(RetryStopReason::AttemptsExhausted);
        }
        if failure.retry_advice() == Some(false) {
            return RetryDecision::DoNotRetry(RetryStopReason::ProviderVeto);
        }
        let provider_approved = failure.retry_advice() == Some(true);
        if !is_transient(failure.class())
            && !(provider_approved && failure.class() == ProviderErrorClass::InvalidRequest)
        {
            return RetryDecision::DoNotRetry(RetryStopReason::NonRetryableClass);
        }
        if matches!(
            failure.class(),
            ProviderErrorClass::Network | ProviderErrorClass::Timeout
        ) && failure.origin() == ProviderFailureOrigin::Transport
            && self.safety != RetrySafety::StatelessPreOutput
        {
            return RetryDecision::DoNotRetry(RetryStopReason::ReplaySafetyUnproven);
        }
        if let Some(hint) = failure.retry_after()
            && let Some(delay) = retry_after_delay(hint, attempt.now_unix_ms)
        {
            if delay > self.max_retry_after {
                return RetryDecision::DoNotRetry(RetryStopReason::RetryAfterExceedsLimit);
            }
            return RetryDecision::Retry {
                next_attempt: attempt.number.saturating_add(1),
                delay,
                source: RetryDelaySource::RetryAfter,
            };
        }
        let backoff = exponential_backoff(self, attempt.number);
        let (delay, source) = match self.jitter {
            RetryJitter::None => (backoff, RetryDelaySource::ExponentialBackoff),
            RetryJitter::Full => {
                let maximum = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
                let sampled = if maximum == u64::MAX {
                    attempt.jitter_sample
                } else {
                    attempt.jitter_sample % maximum.saturating_add(1)
                };
                (
                    Duration::from_millis(sampled),
                    RetryDelaySource::ExponentialBackoffWithJitter,
                )
            }
        };
        RetryDecision::Retry {
            next_attempt: attempt.number.saturating_add(1),
            delay,
            source,
        }
    }
}

fn invalid_retry(field: &'static str, message: &'static str) -> RetrySpecError {
    RetrySpecError::Invalid { field, message }
}

/// Exact state at one failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryAttempt {
    number: u8,
    output_emitted: bool,
    now_unix_ms: u64,
    jitter_sample: u64,
}

impl RetryAttempt {
    /// Validate a one-based attempt number.
    ///
    /// # Errors
    /// Attempt zero is invalid.
    pub fn new(
        number: u8,
        output_emitted: bool,
        now_unix_ms: u64,
        jitter_sample: u64,
    ) -> Result<Self, RetrySpecError> {
        if number == 0 {
            return Err(invalid_retry("attempt", "attempt number must be positive"));
        }
        Ok(Self {
            number,
            output_emitted,
            now_unix_ms,
            jitter_sample,
        })
    }

    fn runtime(number: u8, output_emitted: bool) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok();
        let now_unix_ms = now
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
            .unwrap_or(u64::MAX);
        let nanos = now
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .unwrap_or(now_unix_ms);
        let sequence = JITTER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let jitter_sample =
            mix_jitter(nanos ^ sequence ^ u64::from(number).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        Self {
            number,
            output_emitted,
            now_unix_ms,
            jitter_sample,
        }
    }
}

fn mix_jitter(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// Why the chosen delay exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDelaySource {
    /// Provider `Retry-After` fact.
    RetryAfter,
    /// Exact exponential backoff.
    ExponentialBackoff,
    /// Full-jitter sample bounded by exponential backoff.
    ExponentialBackoffWithJitter,
}

/// Stable reason an automatic replay was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryStopReason {
    /// Policy disables replay.
    Disabled,
    /// Total attempt budget is exhausted.
    AttemptsExhausted,
    /// At least one normalized event was already emitted.
    OutputAlreadyEmitted,
    /// Failure category is permanent.
    NonRetryableClass,
    /// Ambiguous transport failure lacks stateless/idempotent proof.
    ReplaySafetyUnproven,
    /// Provider wait advice exceeds the configured bound.
    RetryAfterExceedsLimit,
    /// Provider explicitly vetoed replay.
    ProviderVeto,
    /// Caller cancellation won.
    Cancelled,
}

/// Testable retry outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Start a fresh attempt after the bounded delay.
    Retry {
        /// One-based next attempt number.
        next_attempt: u8,
        /// Exact delay.
        delay: Duration,
        /// Delay provenance.
        source: RetryDelaySource,
    },
    /// Surface the original failure.
    DoNotRetry(RetryStopReason),
}

/// Map one provider-neutral transport failure into stable body-free facts.
#[must_use]
pub fn classify_transport_error(error: heycode_http::TransportError) -> LlmError {
    let failure = match error {
        heycode_http::TransportError::Http {
            status,
            body,
            metadata,
        } => {
            let code = structured_error_code(body.as_str());
            let status_class = class_from_status(status);
            let body_class = code
                .as_ref()
                .and_then(|code| class_from_code(code.as_str()));
            let guidance = actionable_guidance(status, body.as_str());
            let class =
                if guidance == Some(crate::error::ProviderFailureGuidance::InvalidCredential) {
                    ProviderErrorClass::Authentication
                } else if guidance.is_some() {
                    ProviderErrorClass::InvalidRequest
                } else {
                    refine_invalid_request_class(status_class, body_class)
                };
            let mut failure =
                ProviderFailure::new(class, ProviderFailureOrigin::Http).with_status(status);
            if let Some(guidance) = guidance {
                failure = failure.with_guidance(guidance);
            }
            if let Some(code) = code {
                failure = failure.with_code(code);
            }
            if let Some(retry_after) = metadata.retry_after() {
                failure = failure.with_retry_after(retry_after);
            }
            if let Some(advice) = metadata.should_retry() {
                failure = failure.with_retry_advice(advice);
            }
            failure
        }
        heycode_http::TransportError::Timeout => ProviderFailure::new(
            ProviderErrorClass::Timeout,
            ProviderFailureOrigin::Transport,
        ),
        heycode_http::TransportError::Network { .. } => ProviderFailure::new(
            ProviderErrorClass::Network,
            ProviderFailureOrigin::Transport,
        ),
        heycode_http::TransportError::InvalidSse { .. } => ProviderFailure::new(
            ProviderErrorClass::Protocol,
            ProviderFailureOrigin::Transport,
        ),
        heycode_http::TransportError::InvalidRequest { .. } => {
            ProviderFailure::new(ProviderErrorClass::Protocol, ProviderFailureOrigin::Local)
        }
        heycode_http::TransportError::Cancelled => ProviderFailure::new(
            ProviderErrorClass::Cancelled,
            ProviderFailureOrigin::Transport,
        ),
        heycode_http::TransportError::ResponseTooLarge { .. } => {
            ProviderFailure::new(ProviderErrorClass::Overflow, ProviderFailureOrigin::Local)
        }
        _ => ProviderFailure::new(
            ProviderErrorClass::Protocol,
            ProviderFailureOrigin::Transport,
        ),
    };
    LlmError::Provider(failure)
}

pub(crate) fn provider_event_error(
    default_class: ProviderErrorClass,
    code: Option<&str>,
) -> LlmError {
    let class = code.and_then(class_from_code).unwrap_or(default_class);
    let mut failure = ProviderFailure::new(class, ProviderFailureOrigin::ProviderEvent);
    if let Some(code) = code.and_then(|code| ProviderErrorCode::new(code).ok()) {
        failure = failure.with_code(code);
    }
    LlmError::Provider(failure)
}

pub(crate) fn cancelled_error() -> LlmError {
    LlmError::Provider(ProviderFailure::new(
        ProviderErrorClass::Cancelled,
        ProviderFailureOrigin::Transport,
    ))
}

pub(crate) fn local_failure(class: ProviderErrorClass) -> LlmError {
    LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Local))
}

pub(crate) fn retrying_stream<F>(
    spec: RetrySpec,
    cancellation: CancellationToken,
    mut factory: F,
) -> InferenceStream
where
    F: FnMut(CancellationToken) -> InferenceStream + Send + 'static,
{
    let stream = (!cancellation.is_cancelled()).then(|| factory(cancellation.clone()));
    let state = RetryState {
        spec,
        cancellation,
        factory,
        attempt: 1,
        output_emitted: false,
        stream,
        done: false,
    };
    Box::pin(futures::stream::unfold(state, drive_retry))
}

struct RetryState<F> {
    spec: RetrySpec,
    cancellation: CancellationToken,
    factory: F,
    attempt: u8,
    output_emitted: bool,
    stream: Option<InferenceStream>,
    done: bool,
}

async fn drive_retry<F>(
    mut state: RetryState<F>,
) -> Option<(Result<InferenceEvent, LlmError>, RetryState<F>)>
where
    F: FnMut(CancellationToken) -> InferenceStream + Send + 'static,
{
    loop {
        if state.done {
            return None;
        }
        if state.cancellation.is_cancelled() {
            state.done = true;
            return Some((Err(cancelled_error()), state));
        }
        if state.stream.is_none() {
            state.stream = Some((state.factory)(state.cancellation.clone()));
        }
        let item = {
            let stream = state.stream.as_mut()?;
            tokio::select! {
                biased;
                () = state.cancellation.cancelled() => {
                    state.done = true;
                    return Some((Err(cancelled_error()), state));
                }
                item = stream.next() => item,
            }
        };
        match item {
            Some(Ok(event)) => {
                state.output_emitted = true;
                state.done = matches!(&event, InferenceEvent::Finish(_));
                return Some((Ok(event), state));
            }
            Some(Err(error)) => match state.spec.decide(
                &error,
                RetryAttempt::runtime(state.attempt, state.output_emitted),
            ) {
                RetryDecision::Retry {
                    next_attempt,
                    delay,
                    ..
                } => {
                    tokio::select! {
                        biased;
                        () = state.cancellation.cancelled() => {
                            state.done = true;
                            return Some((Err(cancelled_error()), state));
                        }
                        () = tokio::time::sleep(delay) => {}
                    }
                    state.attempt = next_attempt;
                    state.output_emitted = false;
                    state.stream = None;
                }
                RetryDecision::DoNotRetry(_) => {
                    state.done = true;
                    return Some((Err(error), state));
                }
            },
            None => {
                state.done = true;
                return Some((Err(local_failure(ProviderErrorClass::Protocol)), state));
            }
        }
    }
}

fn is_transient(class: ProviderErrorClass) -> bool {
    matches!(
        class,
        ProviderErrorClass::RateLimited
            | ProviderErrorClass::Overloaded
            | ProviderErrorClass::Server
            | ProviderErrorClass::Timeout
            | ProviderErrorClass::Network
            | ProviderErrorClass::Conflict
    )
}

fn retry_after_delay(hint: heycode_http::HttpRetryAfter, now_unix_ms: u64) -> Option<Duration> {
    match hint {
        heycode_http::HttpRetryAfter::Delay(delay) => Some(delay),
        heycode_http::HttpRetryAfter::At(time) => {
            let at_ms = match time.duration_since(UNIX_EPOCH) {
                Ok(duration) => u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                Err(_) => 0,
            };
            Some(Duration::from_millis(at_ms.saturating_sub(now_unix_ms)))
        }
    }
}

fn exponential_backoff(spec: &RetrySpec, attempt: u8) -> Duration {
    let base = u64::try_from(spec.base_delay.as_millis()).unwrap_or(u64::MAX);
    let maximum = u64::try_from(spec.max_backoff.as_millis()).unwrap_or(u64::MAX);
    let multiplier = 1_u64
        .checked_shl(u32::from(attempt.saturating_sub(1)))
        .unwrap_or(u64::MAX);
    Duration::from_millis(base.saturating_mul(multiplier).min(maximum))
}

fn actionable_guidance(status: u16, body: &str) -> Option<crate::error::ProviderFailureGuidance> {
    use crate::error::ProviderFailureGuidance as Hint;
    // Only inspect a structured provider message. Never retain or render it:
    // providers can echo prompts, credentials and upstream response bodies.
    let value = serde_json::from_str::<serde_json::Value>(body).ok();
    let message = value
        .as_ref()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .or_else(|| value.get("message"))
        })
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let code = value
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match status {
        401 => Some(Hint::InvalidCredential),
        403 if code == "invalid_api_key" || message.contains("invalid api key") => {
            Some(Hint::InvalidCredential)
        }
        429 if matches!(code, "insufficient_quota" | "billing_hard_limit_reached") => {
            Some(Hint::Credits)
        }
        402 => Some(Hint::Credits),
        403 if message.contains("age confirmation") || message.contains("age verification") => {
            Some(
                if message.contains("https://openrouter.ai/settings/preferences") {
                    Hint::OpenRouterAgeConfirmation
                } else {
                    Hint::AgeConfirmation
                },
            )
        }
        403 if message.contains("data policy") || message.contains("data collection") => {
            Some(Hint::DataPolicy)
        }
        403 => Some(Hint::AccessDenied),
        400 | 404
            if message.contains("model")
                && [
                    "not found",
                    "does not exist",
                    "not available",
                    "unavailable",
                ]
                .iter()
                .any(|needle| message.contains(needle)) =>
        {
            Some(Hint::ModelUnavailable)
        }
        400 | 422
            if (message.contains("unsupported") || message.contains("not supported"))
                && ["parameter", "feature", "argument"]
                    .iter()
                    .any(|needle| message.contains(needle)) =>
        {
            let parameter = [
                "temperature",
                "top_p",
                "max_tokens",
                "max_completion_tokens",
                "reasoning_effort",
                "tool_choice",
                "tools",
                "response_format",
            ]
            .into_iter()
            .find(|parameter| message.contains(parameter));
            Some(parameter.map_or(Hint::UnsupportedRequest, Hint::UnsupportedParameter))
        }
        _ => None,
    }
}

fn structured_error_code(body: &str) -> Option<ProviderErrorCode> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let root = value.as_object()?;
    let error = root.get("error").and_then(serde_json::Value::as_object);
    let candidate = error
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            error
                .and_then(|error| error.get("type"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| root.get("code").and_then(serde_json::Value::as_str))
        .or_else(|| root.get("type").and_then(serde_json::Value::as_str));
    candidate.and_then(|code| ProviderErrorCode::new(code).ok())
}

fn class_from_code(code: &str) -> Option<ProviderErrorClass> {
    let normalized = code.to_ascii_lowercase();
    if normalized.contains("context")
        && (normalized.contains("length")
            || normalized.contains("window")
            || normalized.contains("token"))
    {
        Some(ProviderErrorClass::ContextWindowExceeded)
    } else if matches!(
        normalized.as_str(),
        "authentication_error" | "invalid_api_key" | "unauthorized" | "permission_error"
    ) {
        Some(ProviderErrorClass::Authentication)
    } else if matches!(
        normalized.as_str(),
        "cancelled" | "canceled" | "cancelled_error"
    ) {
        Some(ProviderErrorClass::Cancelled)
    } else if normalized.contains("rate_limit") || normalized == "insufficient_quota" {
        Some(ProviderErrorClass::RateLimited)
    } else if normalized.contains("overload") {
        Some(ProviderErrorClass::Overloaded)
    } else if normalized.contains("timeout") {
        Some(ProviderErrorClass::Timeout)
    } else if normalized.contains("too_large") || normalized.contains("overflow") {
        Some(ProviderErrorClass::Overflow)
    } else if normalized.contains("server_error") || normalized == "api_error" {
        Some(ProviderErrorClass::Server)
    } else if normalized.contains("conflict") {
        Some(ProviderErrorClass::Conflict)
    } else if matches!(
        normalized.as_str(),
        "invalid_request_error"
            | "bad_request"
            | "unprocessable_entity"
            | "not_found_error"
            | "billing_error"
    ) {
        Some(ProviderErrorClass::InvalidRequest)
    } else {
        None
    }
}

const fn class_from_status(status: u16) -> ProviderErrorClass {
    match status {
        401 | 403 => ProviderErrorClass::Authentication,
        408 | 504 => ProviderErrorClass::Timeout,
        409 => ProviderErrorClass::Conflict,
        413 => ProviderErrorClass::Overflow,
        429 => ProviderErrorClass::RateLimited,
        503 | 529 => ProviderErrorClass::Overloaded,
        500..=599 => ProviderErrorClass::Server,
        _ => ProviderErrorClass::InvalidRequest,
    }
}

const fn refine_invalid_request_class(
    status_class: ProviderErrorClass,
    body_class: Option<ProviderErrorClass>,
) -> ProviderErrorClass {
    if !matches!(status_class, ProviderErrorClass::InvalidRequest) {
        return status_class;
    }
    match body_class {
        Some(ProviderErrorClass::ContextWindowExceeded) => {
            ProviderErrorClass::ContextWindowExceeded
        }
        Some(ProviderErrorClass::Overflow) => ProviderErrorClass::Overflow,
        _ => ProviderErrorClass::InvalidRequest,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::StreamExt as _;

    use super::*;

    #[tokio::test]
    async fn finish_is_terminal_before_cancellation_or_another_inner_poll() {
        let polls = Arc::new(AtomicUsize::new(0));
        let inner_polls = polls.clone();
        let cancellation = CancellationToken::new();
        let stream = retrying_stream(RetrySpec::no_retry(), cancellation.clone(), move |_| {
            let inner_polls = inner_polls.clone();
            Box::pin(futures::stream::poll_fn(move |_| {
                let poll = inner_polls.fetch_add(1, Ordering::SeqCst);
                if poll == 0 {
                    std::task::Poll::Ready(Some(Ok(InferenceEvent::Finish(
                        crate::FinishReason::Stop,
                    ))))
                } else {
                    std::task::Poll::Ready(Some(Err(local_failure(ProviderErrorClass::Protocol))))
                }
            }))
        });
        futures::pin_mut!(stream);

        assert!(matches!(
            stream.next().await,
            Some(Ok(InferenceEvent::Finish(crate::FinishReason::Stop)))
        ));
        cancellation.cancel();
        assert!(stream.next().await.is_none());
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }
}

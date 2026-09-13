//! Bounded cross-session activity facts projected from durable JSONL truth.
//!
//! The local query backend supplies only each physical session suffix. That is
//! important for shared-prefix forks: inherited events are useful when one
//! session resumes, but counting them again in every descendant inflates
//! activity and usage. This projection is provider-neutral and prices nothing.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Days, NaiveDate, Timelike as _, Utc};

use crate::{SessionEvent, SessionEventKind, SessionStorageState};

/// Activity observed on one UTC date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatsDay {
    date: String,
    sessions: u64,
    user_messages: u64,
    assistant_messages: u64,
    reported_input_tokens: u128,
    reported_output_tokens: u128,
}

impl SessionStatsDay {
    /// ISO-8601 UTC calendar date.
    #[must_use]
    pub fn date(&self) -> &str {
        &self.date
    }

    /// Physical sessions with at least one message on this date.
    #[must_use]
    pub const fn sessions(&self) -> u64 {
        self.sessions
    }

    /// User messages committed on this date.
    #[must_use]
    pub const fn user_messages(&self) -> u64 {
        self.user_messages
    }

    /// Assistant messages committed on this date.
    #[must_use]
    pub const fn assistant_messages(&self) -> u64 {
        self.assistant_messages
    }

    /// Provider-reported input tokens on this date.
    #[must_use]
    pub const fn reported_input_tokens(&self) -> u128 {
        self.reported_input_tokens
    }

    /// Provider-reported output tokens on this date.
    #[must_use]
    pub const fn reported_output_tokens(&self) -> u128 {
        self.reported_output_tokens
    }
}

/// Provider/model activity attributable to durable request headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionModelStats {
    provider: String,
    model: String,
    responses: u64,
    reported_responses: u64,
    reported_input_tokens: u128,
    reported_output_tokens: u128,
}

impl SessionModelStats {
    /// Provider id from the request header.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Model id from the request header.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Assistant messages attributable to this route.
    #[must_use]
    pub const fn responses(&self) -> u64 {
        self.responses
    }

    /// Attributable responses that included provider token usage.
    #[must_use]
    pub const fn reported_responses(&self) -> u64 {
        self.reported_responses
    }

    /// Provider-reported input tokens attributable to this route.
    #[must_use]
    pub const fn reported_input_tokens(&self) -> u128 {
        self.reported_input_tokens
    }

    /// Provider-reported output tokens attributable to this route.
    #[must_use]
    pub const fn reported_output_tokens(&self) -> u128 {
        self.reported_output_tokens
    }
}

/// One complete bounded cross-session statistics scan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionStatsSnapshot {
    readable_sessions: u64,
    unreadable_sessions: u64,
    used_sessions: u64,
    active_used_sessions: u64,
    archived_used_sessions: u64,
    user_messages: u64,
    assistant_messages: u64,
    first_activity_ms: Option<i64>,
    last_activity_ms: Option<i64>,
    calendar_days: u64,
    active_days: u64,
    current_streak_days: u64,
    longest_streak_days: u64,
    peak_hour_utc: Option<u8>,
    recorded_session_spans: u64,
    longest_session_span_ms: Option<u64>,
    reported_input_tokens: u128,
    reported_output_tokens: u128,
    unreported_assistant_messages: u64,
    unattributed_assistant_messages: u64,
    cache_read_tokens: u128,
    cache_write_tokens: u128,
    cache_reports: u64,
    unknown_cache_write_reports: u64,
    days: Vec<SessionStatsDay>,
    models: Vec<SessionModelStats>,
}

impl SessionStatsSnapshot {
    /// Whether the readable store contains no used physical sessions.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.used_sessions == 0 && self.unreadable_sessions == 0
    }

    /// Physical logs this build opened and validated.
    #[must_use]
    pub const fn readable_sessions(&self) -> u64 {
        self.readable_sessions
    }

    /// Session-shaped logs that could not be projected.
    #[must_use]
    pub const fn unreadable_sessions(&self) -> u64 {
        self.unreadable_sessions
    }

    /// Readable physical sessions containing work or fork lineage.
    #[must_use]
    pub const fn used_sessions(&self) -> u64 {
        self.used_sessions
    }

    /// Used sessions without an archive marker.
    #[must_use]
    pub const fn active_used_sessions(&self) -> u64 {
        self.active_used_sessions
    }

    /// Used sessions with a valid archive marker.
    #[must_use]
    pub const fn archived_used_sessions(&self) -> u64 {
        self.archived_used_sessions
    }

    /// Durable user message count.
    #[must_use]
    pub const fn user_messages(&self) -> u64 {
        self.user_messages
    }

    /// Durable assistant message count.
    #[must_use]
    pub const fn assistant_messages(&self) -> u64 {
        self.assistant_messages
    }

    /// Durable user plus assistant messages.
    #[must_use]
    pub const fn total_messages(&self) -> u64 {
        self.user_messages.saturating_add(self.assistant_messages)
    }

    /// First UTC message commit time, when representable.
    #[must_use]
    pub const fn first_activity_ms(&self) -> Option<i64> {
        self.first_activity_ms
    }

    /// Latest UTC message commit time, when representable.
    #[must_use]
    pub const fn last_activity_ms(&self) -> Option<i64> {
        self.last_activity_ms
    }

    /// Inclusive calendar-day range between first and last activity.
    #[must_use]
    pub const fn calendar_days(&self) -> u64 {
        self.calendar_days
    }

    /// Distinct UTC dates with at least one durable message.
    #[must_use]
    pub const fn active_days(&self) -> u64 {
        self.active_days
    }

    /// Consecutive UTC activity days ending today or yesterday.
    #[must_use]
    pub const fn current_streak_days(&self) -> u64 {
        self.current_streak_days
    }

    /// Longest consecutive UTC activity-day run.
    #[must_use]
    pub const fn longest_streak_days(&self) -> u64 {
        self.longest_streak_days
    }

    /// UTC hour containing the most messages; earliest wins a tie.
    #[must_use]
    pub const fn peak_hour_utc(&self) -> Option<u8> {
        self.peak_hour_utc
    }

    /// Used sessions with at least one timestamped message.
    #[must_use]
    pub const fn recorded_session_spans(&self) -> u64 {
        self.recorded_session_spans
    }

    /// Longest first-to-last message span within one physical session.
    #[must_use]
    pub const fn longest_session_span_ms(&self) -> Option<u64> {
        self.longest_session_span_ms
    }

    /// Sum of provider-reported input tokens.
    #[must_use]
    pub const fn reported_input_tokens(&self) -> u128 {
        self.reported_input_tokens
    }

    /// Sum of provider-reported output tokens.
    #[must_use]
    pub const fn reported_output_tokens(&self) -> u128 {
        self.reported_output_tokens
    }

    /// Assistant messages whose provider omitted token usage.
    #[must_use]
    pub const fn unreported_assistant_messages(&self) -> u64 {
        self.unreported_assistant_messages
    }

    /// Assistant messages without a matching local request-header route.
    #[must_use]
    pub const fn unattributed_assistant_messages(&self) -> u64 {
        self.unattributed_assistant_messages
    }

    /// Sum of detailed provider-reported cache reads.
    #[must_use]
    pub const fn cache_read_tokens(&self) -> u128 {
        self.cache_read_tokens
    }

    /// Sum of known detailed provider-reported cache writes.
    #[must_use]
    pub const fn cache_write_tokens(&self) -> u128 {
        self.cache_write_tokens
    }

    /// Responses carrying detailed cache counters.
    #[must_use]
    pub const fn cache_reports(&self) -> u64 {
        self.cache_reports
    }

    /// Detailed cache reports where write tokens were omitted.
    #[must_use]
    pub const fn unknown_cache_write_reports(&self) -> u64 {
        self.unknown_cache_write_reports
    }

    /// Activity dates in ascending order.
    #[must_use]
    pub fn days(&self) -> &[SessionStatsDay] {
        &self.days
    }

    /// Provider/model rows ordered by response count then token volume.
    #[must_use]
    pub fn models(&self) -> &[SessionModelStats] {
        &self.models
    }
}

#[derive(Default)]
struct MutableDay {
    sessions: u64,
    user_messages: u64,
    assistant_messages: u64,
    reported_input_tokens: u128,
    reported_output_tokens: u128,
}

#[derive(Default)]
struct MutableModel {
    responses: u64,
    reported_responses: u64,
    reported_input_tokens: u128,
    reported_output_tokens: u128,
}

#[derive(Default)]
pub(crate) struct SessionStatsAccumulator {
    snapshot: SessionStatsSnapshot,
    days: BTreeMap<NaiveDate, MutableDay>,
    hours: [u64; 24],
    models: BTreeMap<(String, String), MutableModel>,
}

impl SessionStatsAccumulator {
    pub(crate) fn unreadable(&mut self) {
        self.snapshot.unreadable_sessions = self.snapshot.unreadable_sessions.saturating_add(1);
    }

    pub(crate) fn observe(
        &mut self,
        events: &[SessionEvent],
        lineage: bool,
        storage: SessionStorageState,
    ) {
        self.snapshot.readable_sessions = self.snapshot.readable_sessions.saturating_add(1);
        let used = lineage || !crate::is_unused_session(events);
        if !used {
            return;
        }
        self.snapshot.used_sessions = self.snapshot.used_sessions.saturating_add(1);
        match storage {
            SessionStorageState::Active => {
                self.snapshot.active_used_sessions =
                    self.snapshot.active_used_sessions.saturating_add(1);
            }
            SessionStorageState::Archived => {
                self.snapshot.archived_used_sessions =
                    self.snapshot.archived_used_sessions.saturating_add(1);
            }
        }

        let mut routes = BTreeMap::<(u64, u32), (String, String)>::new();
        let mut turn_routes = BTreeMap::<u64, (String, String)>::new();
        let mut session_days = BTreeSet::<NaiveDate>::new();
        let mut first_message_ms = None::<i64>;
        let mut last_message_ms = None::<i64>;

        for event in events {
            if let SessionEventKind::RequestHeader {
                turn, step, header, ..
            } = &event.kind
            {
                let route = (header.provider.clone(), header.model.clone());
                routes.insert((*turn, *step), route.clone());
                turn_routes.insert(*turn, route);
            }

            let message_kind = match &event.kind {
                SessionEventKind::UserMessage { .. } => Some(true),
                SessionEventKind::AssistantMessage { .. } => Some(false),
                _ => None,
            };
            let timestamp = DateTime::<Utc>::from_timestamp_millis(event.time_ms);
            if let (Some(user), Some(timestamp)) = (message_kind, timestamp) {
                let date = timestamp.date_naive();
                let hour = usize::try_from(timestamp.hour()).unwrap_or(0);
                session_days.insert(date);
                self.hours[hour] = self.hours[hour].saturating_add(1);
                let day = self.days.entry(date).or_default();
                if user {
                    self.snapshot.user_messages = self.snapshot.user_messages.saturating_add(1);
                    day.user_messages = day.user_messages.saturating_add(1);
                } else {
                    self.snapshot.assistant_messages =
                        self.snapshot.assistant_messages.saturating_add(1);
                    day.assistant_messages = day.assistant_messages.saturating_add(1);
                }
                first_message_ms =
                    Some(first_message_ms.map_or(event.time_ms, |value| value.min(event.time_ms)));
                last_message_ms =
                    Some(last_message_ms.map_or(event.time_ms, |value| value.max(event.time_ms)));
                self.snapshot.first_activity_ms = Some(
                    self.snapshot
                        .first_activity_ms
                        .map_or(event.time_ms, |value| value.min(event.time_ms)),
                );
                self.snapshot.last_activity_ms = Some(
                    self.snapshot
                        .last_activity_ms
                        .map_or(event.time_ms, |value| value.max(event.time_ms)),
                );
            }

            if let SessionEventKind::AssistantMessage {
                turn, step, usage, ..
            } = &event.kind
            {
                // Normal records match the exact step. Legacy/native adapter
                // logs may number the producing message differently, while
                // still preserving the latest request route within that turn; this
                // is the same conservative fallback used by project_usage.
                let route = routes
                    .get(&(*turn, *step))
                    .or_else(|| turn_routes.get(turn));
                if let Some((provider, model)) = route {
                    let row = self
                        .models
                        .entry((provider.clone(), model.clone()))
                        .or_default();
                    row.responses = row.responses.saturating_add(1);
                } else {
                    self.snapshot.unattributed_assistant_messages = self
                        .snapshot
                        .unattributed_assistant_messages
                        .saturating_add(1);
                }
                if let Some(usage) = usage {
                    let input = u128::from(usage.prompt_tokens);
                    let output = u128::from(usage.completion_tokens);
                    self.snapshot.reported_input_tokens =
                        self.snapshot.reported_input_tokens.saturating_add(input);
                    self.snapshot.reported_output_tokens =
                        self.snapshot.reported_output_tokens.saturating_add(output);
                    if let Some(timestamp) = timestamp {
                        let day = self.days.entry(timestamp.date_naive()).or_default();
                        day.reported_input_tokens = day.reported_input_tokens.saturating_add(input);
                        day.reported_output_tokens =
                            day.reported_output_tokens.saturating_add(output);
                    }
                    if let Some((provider, model)) = route {
                        let model = self
                            .models
                            .entry((provider.clone(), model.clone()))
                            .or_default();
                        model.reported_responses = model.reported_responses.saturating_add(1);
                        model.reported_input_tokens =
                            model.reported_input_tokens.saturating_add(input);
                        model.reported_output_tokens =
                            model.reported_output_tokens.saturating_add(output);
                    }
                } else {
                    self.snapshot.unreported_assistant_messages = self
                        .snapshot
                        .unreported_assistant_messages
                        .saturating_add(1);
                }
            }

            if let SessionEventKind::AssistantResponseMetadata { metadata, .. } = &event.kind
                && let Some(cache) = metadata.cache_usage()
            {
                self.snapshot.cache_reports = self.snapshot.cache_reports.saturating_add(1);
                self.snapshot.cache_read_tokens = self
                    .snapshot
                    .cache_read_tokens
                    .saturating_add(u128::from(cache.cache_read_tokens()));
                if let Some(writes) = cache.reported_cache_write_tokens() {
                    self.snapshot.cache_write_tokens = self
                        .snapshot
                        .cache_write_tokens
                        .saturating_add(u128::from(writes));
                } else {
                    self.snapshot.unknown_cache_write_reports =
                        self.snapshot.unknown_cache_write_reports.saturating_add(1);
                }
            }
        }

        for date in session_days {
            let day = self.days.entry(date).or_default();
            day.sessions = day.sessions.saturating_add(1);
        }
        if let (Some(first), Some(last)) = (first_message_ms, last_message_ms) {
            self.snapshot.recorded_session_spans =
                self.snapshot.recorded_session_spans.saturating_add(1);
            let span = u64::try_from(last.saturating_sub(first)).unwrap_or(0);
            self.snapshot.longest_session_span_ms = Some(
                self.snapshot
                    .longest_session_span_ms
                    .map_or(span, |current| current.max(span)),
            );
        }
    }

    pub(crate) fn finish(mut self) -> SessionStatsSnapshot {
        let dates = self.days.keys().copied().collect::<Vec<_>>();
        self.snapshot.active_days = u64::try_from(dates.len()).unwrap_or(u64::MAX);
        if let (Some(first), Some(last)) = (dates.first(), dates.last()) {
            self.snapshot.calendar_days = u64::try_from(
                last.signed_duration_since(*first)
                    .num_days()
                    .saturating_add(1),
            )
            .unwrap_or(0);
            let mut longest = 1_u64;
            let mut run = 1_u64;
            for pair in dates.windows(2) {
                if pair[0].checked_add_days(Days::new(1)) == Some(pair[1]) {
                    run = run.saturating_add(1);
                    longest = longest.max(run);
                } else {
                    run = 1;
                }
            }
            self.snapshot.longest_streak_days = longest;
            let today = Utc::now().date_naive();
            if *last == today || last.checked_add_days(Days::new(1)) == Some(today) {
                self.snapshot.current_streak_days = run;
            }
        }
        self.snapshot.peak_hour_utc = self
            .hours
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(&left.0)))
            .filter(|(_, count)| **count > 0)
            .and_then(|(hour, _)| u8::try_from(hour).ok());
        self.snapshot.days = self
            .days
            .into_iter()
            .map(|(date, day)| SessionStatsDay {
                date: date.format("%Y-%m-%d").to_string(),
                sessions: day.sessions,
                user_messages: day.user_messages,
                assistant_messages: day.assistant_messages,
                reported_input_tokens: day.reported_input_tokens,
                reported_output_tokens: day.reported_output_tokens,
            })
            .collect();
        self.snapshot.models = self
            .models
            .into_iter()
            .map(|((provider, model), row)| SessionModelStats {
                provider,
                model,
                responses: row.responses,
                reported_responses: row.reported_responses,
                reported_input_tokens: row.reported_input_tokens,
                reported_output_tokens: row.reported_output_tokens,
            })
            .collect();
        self.snapshot.models.sort_by(|left, right| {
            right
                .responses
                .cmp(&left.responses)
                .then_with(|| {
                    right
                        .reported_input_tokens
                        .saturating_add(right.reported_output_tokens)
                        .cmp(
                            &left
                                .reported_input_tokens
                                .saturating_add(left.reported_output_tokens),
                        )
                })
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| left.model.cmp(&right.model))
        });
        self.snapshot
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::{Duration, NaiveDateTime};
    use heycode_core::ProviderProtocol;

    use super::*;

    fn timestamp(value: NaiveDateTime) -> i64 {
        value.and_utc().timestamp_millis()
    }

    fn event(seq: u64, time_ms: i64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            v: crate::CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms,
            kind,
        }
    }

    fn header() -> crate::RequestHeaderSnapshot {
        crate::RequestHeaderSnapshot::new(
            "provider-a",
            "model-a",
            ProviderProtocol::OpenAiChatCompletions,
            crate::RequestTargetSnapshot::Http {
                base_url: "https://example.test/v1".to_owned(),
            },
            crate::RequestAuthenticationSnapshot::None,
            None,
            Vec::new(),
            crate::RequestOptionsSnapshot {
                input_modalities: vec!["text".to_owned()],
                reasoning_effort: None,
                defaulted_reasoning_effort: false,
                structured_output: None,
                native_features: Vec::new(),
                native_tool_routes: Vec::new(),
                provider_options: Vec::new(),
                temperature: None,
                max_output_tokens: None,
                defaulted_max_output_tokens: false,
                purpose: "conversation".to_owned(),
                retry: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn projection_groups_utc_days_streaks_routes_and_unknown_evidence() {
        let today = Utc::now().date_naive();
        let old = today.checked_sub_signed(Duration::days(3)).unwrap();
        let yesterday = today.checked_sub_signed(Duration::days(1)).unwrap();
        let old_noon = timestamp(old.and_hms_opt(12, 0, 0).unwrap());
        let yesterday_noon = timestamp(yesterday.and_hms_opt(12, 0, 0).unwrap());
        let today_noon = timestamp(today.and_hms_opt(12, 0, 0).unwrap());
        let request_id = heycode_core::RequestId::from_raw("stats_test_request");
        let events = vec![
            event(
                0,
                old_noon,
                SessionEventKind::UserMessage {
                    text: "old".to_owned(),
                },
            ),
            event(
                1,
                yesterday_noon,
                SessionEventKind::UserMessage {
                    text: "recent".to_owned(),
                },
            ),
            event(
                2,
                yesterday_noon,
                SessionEventKind::RequestHeader {
                    turn: 1,
                    step: 0,
                    request_id: request_id.clone(),
                    header: Box::new(header()),
                },
            ),
            event(
                3,
                yesterday_noon,
                SessionEventKind::AssistantMessage {
                    turn: 1,
                    step: 0,
                    content: "reported".to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: Some(heycode_core::TokenUsage {
                        prompt_tokens: 20,
                        completion_tokens: 5,
                    }),
                },
            ),
            event(
                4,
                yesterday_noon,
                SessionEventKind::AssistantResponseMetadata {
                    turn: 1,
                    step: 0,
                    request_id,
                    metadata: Box::new(
                        heycode_core::ProviderResponseMetadata::new(
                            Some(
                                heycode_core::ProviderCacheUsage::new(20, 5, 8, 0)
                                    .unwrap()
                                    .with_unknown_cache_writes(),
                            ),
                            Vec::new(),
                            None,
                        )
                        .unwrap(),
                    ),
                },
            ),
            event(
                5,
                today_noon,
                SessionEventKind::AssistantMessage {
                    turn: 2,
                    step: 0,
                    content: "unreported".to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                },
            ),
        ];
        let mut accumulator = SessionStatsAccumulator::default();
        accumulator.observe(&events, false, SessionStorageState::Active);
        let snapshot = accumulator.finish();

        assert_eq!(snapshot.active_days(), 3);
        assert_eq!(snapshot.calendar_days(), 4);
        assert_eq!(snapshot.current_streak_days(), 2);
        assert_eq!(snapshot.longest_streak_days(), 2);
        assert_eq!(snapshot.peak_hour_utc(), Some(12));
        assert_eq!(snapshot.user_messages(), 2);
        assert_eq!(snapshot.assistant_messages(), 2);
        assert_eq!(snapshot.reported_input_tokens(), 20);
        assert_eq!(snapshot.reported_output_tokens(), 5);
        assert_eq!(snapshot.unreported_assistant_messages(), 1);
        assert_eq!(snapshot.unattributed_assistant_messages(), 1);
        assert_eq!(snapshot.cache_read_tokens(), 8);
        assert_eq!(snapshot.cache_write_tokens(), 0);
        assert_eq!(snapshot.unknown_cache_write_reports(), 1);
        assert_eq!(snapshot.models().len(), 1);
        assert_eq!(snapshot.models()[0].responses(), 1);
        assert_eq!(snapshot.days()[1].reported_input_tokens(), 20);
        assert_eq!(snapshot.recorded_session_spans(), 1);
        assert!(snapshot.longest_session_span_ms().unwrap() > 0);
    }
}

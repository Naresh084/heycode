//! Provider-neutral view model for the source-shaped Stats overview.
//!
//! Durable statistics stay owned by `heycode-session`. This module turns that
//! immutable snapshot into the two interactive presentation children used by
//! the TUI: Overview and Models. It deliberately carries missing evidence as
//! `None` or a lower-bound marker; subscription limits, billing and inferred
//! per-range model/session totals are never synthesized.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike as _, Days, NaiveDate};

/// Source-shaped child within the Stats tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsViewTab {
    /// Activity heatmap and summary facts.
    Overview,
    /// Provider/model attribution table.
    Models,
}

impl StatsViewTab {
    /// Human label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Models => "Models",
        }
    }

    /// Toggle to the other child.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Overview => Self::Models,
            Self::Models => Self::Overview,
        }
    }
}

/// Date scope cycled by `r` in the Overview child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsDateRange {
    /// Complete bounded local history, charted over the latest 53 weeks.
    AllTime,
    /// Latest seven UTC calendar days.
    LastSevenDays,
    /// Latest thirty UTC calendar days.
    LastThirtyDays,
}

impl StatsDateRange {
    /// Source-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AllTime => "All time",
            Self::LastSevenDays => "Last 7 days",
            Self::LastThirtyDays => "Last 30 days",
        }
    }

    /// Next range in the source control's displayed order.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::AllTime => Self::LastSevenDays,
            Self::LastSevenDays => Self::LastThirtyDays,
            Self::LastThirtyDays => Self::AllTime,
        }
    }

    const fn days(self) -> Option<u64> {
        match self {
            Self::AllTime => None,
            Self::LastSevenDays => Some(7),
            Self::LastThirtyDays => Some(30),
        }
    }

    const fn chart_weeks(self) -> u64 {
        match self {
            Self::AllTime => 53,
            Self::LastSevenDays => 1,
            Self::LastThirtyDays => 5,
        }
    }
}

/// A count whose source evidence may be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsNumber {
    value: u128,
    lower_bound: bool,
}

impl StatsNumber {
    pub(crate) const fn new(value: u128, lower_bound: bool) -> Self {
        Self { value, lower_bound }
    }

    /// Observed value.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.value
    }

    /// Whether the observed value must be labelled as at least this amount.
    #[must_use]
    pub const fn is_lower_bound(self) -> bool {
        self.lower_bound
    }
}

/// One UTC day in the activity heatmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsHeatmapCell {
    date: NaiveDate,
    messages: u64,
    sessions: u64,
    intensity: u8,
}

impl StatsHeatmapCell {
    /// UTC date.
    #[must_use]
    pub const fn date(&self) -> NaiveDate {
        self.date
    }

    /// Durable user plus assistant messages.
    #[must_use]
    pub const fn messages(&self) -> u64 {
        self.messages
    }

    /// Physical sessions active on this date.
    #[must_use]
    pub const fn sessions(&self) -> u64 {
        self.sessions
    }

    /// Relative activity bucket from 0 (none) through 4 (most).
    #[must_use]
    pub const fn intensity(&self) -> u8 {
        self.intensity
    }
}

/// Seven Monday-through-Sunday cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsHeatmapWeek {
    cells: [StatsHeatmapCell; 7],
}

impl StatsHeatmapWeek {
    /// Monday-through-Sunday cells.
    #[must_use]
    pub const fn cells(&self) -> &[StatsHeatmapCell; 7] {
        &self.cells
    }
}

/// Month label anchored to one heatmap week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsMonthMarker {
    week: usize,
    label: String,
}

impl StatsMonthMarker {
    /// Zero-based heatmap week index.
    #[must_use]
    pub const fn week(&self) -> usize {
        self.week
    }

    /// Three-letter English month label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Complete Overview heatmap geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsHeatmap {
    start: NaiveDate,
    end: NaiveDate,
    weeks: Vec<StatsHeatmapWeek>,
    months: Vec<StatsMonthMarker>,
}

impl StatsHeatmap {
    /// First charted UTC date (a Monday).
    #[must_use]
    pub const fn start(&self) -> NaiveDate {
        self.start
    }

    /// Last charted UTC date (a Sunday).
    #[must_use]
    pub const fn end(&self) -> NaiveDate {
        self.end
    }

    /// Week columns in chronological order.
    #[must_use]
    pub fn weeks(&self) -> &[StatsHeatmapWeek] {
        &self.weeks
    }

    /// Sparse month labels for the week header.
    #[must_use]
    pub fn months(&self) -> &[StatsMonthMarker] {
        &self.months
    }
}

/// Source-shaped Overview summary backed only by local durable facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsOverviewSummary {
    favorite_model: Option<String>,
    total_tokens: StatsNumber,
    input_tokens: StatsNumber,
    output_tokens: StatsNumber,
    cache_read_tokens: StatsNumber,
    cache_write_tokens: StatsNumber,
    sessions: Option<u64>,
    active_sessions: Option<u64>,
    archived_sessions: Option<u64>,
    active_days: u64,
    calendar_days: u64,
    current_streak_days: u64,
    longest_streak_days: u64,
    most_active_day: Option<(NaiveDate, u64)>,
    longest_session_ms: Option<u64>,
    total_messages: u64,
    unreadable_sessions: u64,
    unreported_responses: u64,
    unattributed_responses: u64,
}

impl StatsOverviewSummary {
    /// Most-used attributable provider/model, all-time only.
    #[must_use]
    pub fn favorite_model(&self) -> Option<&str> {
        self.favorite_model.as_deref()
    }

    /// Reported input plus output tokens in the selected date range.
    #[must_use]
    pub const fn total_tokens(&self) -> StatsNumber {
        self.total_tokens
    }

    /// Provider-reported input tokens in the selected date range.
    #[must_use]
    pub const fn input_tokens(&self) -> StatsNumber {
        self.input_tokens
    }

    /// Provider-reported output tokens in the selected date range.
    #[must_use]
    pub const fn output_tokens(&self) -> StatsNumber {
        self.output_tokens
    }

    /// Detailed provider-reported cache reads. Date filtering is unavailable.
    #[must_use]
    pub const fn cache_read_tokens(&self) -> StatsNumber {
        self.cache_read_tokens
    }

    /// Known detailed cache writes. Date filtering is unavailable.
    #[must_use]
    pub const fn cache_write_tokens(&self) -> StatsNumber {
        self.cache_write_tokens
    }

    /// Unique used physical sessions; unavailable for filtered ranges because
    /// per-day unique-session identities are not retained by the aggregate.
    #[must_use]
    pub const fn sessions(&self) -> Option<u64> {
        self.sessions
    }

    /// Active used sessions, all-time only.
    #[must_use]
    pub const fn active_sessions(&self) -> Option<u64> {
        self.active_sessions
    }

    /// Archived used sessions, all-time only.
    #[must_use]
    pub const fn archived_sessions(&self) -> Option<u64> {
        self.archived_sessions
    }

    /// UTC dates with activity in the selected range.
    #[must_use]
    pub const fn active_days(&self) -> u64 {
        self.active_days
    }

    /// Calendar-day denominator for the selected range.
    #[must_use]
    pub const fn calendar_days(&self) -> u64 {
        self.calendar_days
    }

    /// Current consecutive UTC activity days in the selected range.
    #[must_use]
    pub const fn current_streak_days(&self) -> u64 {
        self.current_streak_days
    }

    /// Longest consecutive UTC activity run in the selected range.
    #[must_use]
    pub const fn longest_streak_days(&self) -> u64 {
        self.longest_streak_days
    }

    /// Busiest observed UTC day and its durable message count.
    #[must_use]
    pub const fn most_active_day(&self) -> Option<(NaiveDate, u64)> {
        self.most_active_day
    }

    /// Longest recorded first-to-last message span, all-time only.
    #[must_use]
    pub const fn longest_session_ms(&self) -> Option<u64> {
        self.longest_session_ms
    }

    /// Durable messages in the selected range.
    #[must_use]
    pub const fn total_messages(&self) -> u64 {
        self.total_messages
    }

    /// Logs excluded from the bounded scan.
    #[must_use]
    pub const fn unreadable_sessions(&self) -> u64 {
        self.unreadable_sessions
    }

    /// Assistant responses without provider token usage.
    #[must_use]
    pub const fn unreported_responses(&self) -> u64 {
        self.unreported_responses
    }

    /// Assistant responses without a local request-header route.
    #[must_use]
    pub const fn unattributed_responses(&self) -> u64 {
        self.unattributed_responses
    }
}

/// One all-time provider/model row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsModelView {
    provider: String,
    model: String,
    responses: u64,
    reported_responses: u64,
    input_tokens: u128,
    output_tokens: u128,
}

impl StatsModelView {
    /// Provider registry id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Provider-native model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Attributable assistant responses.
    #[must_use]
    pub const fn responses(&self) -> u64 {
        self.responses
    }

    /// Responses carrying provider token usage.
    #[must_use]
    pub const fn reported_responses(&self) -> u64 {
        self.reported_responses
    }

    /// Provider-reported input tokens.
    #[must_use]
    pub const fn input_tokens(&self) -> u128 {
        self.input_tokens
    }

    /// Provider-reported output tokens.
    #[must_use]
    pub const fn output_tokens(&self) -> u128 {
        self.output_tokens
    }

    /// Provider-reported input plus output tokens.
    #[must_use]
    pub const fn total_tokens(&self) -> u128 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Whether every attributable response carried usage.
    #[must_use]
    pub const fn usage_complete(&self) -> bool {
        self.responses == self.reported_responses
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DayFacts {
    sessions: u64,
    messages: u64,
    input_tokens: u128,
    output_tokens: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatsFacts {
    used_sessions: u64,
    active_sessions: u64,
    archived_sessions: u64,
    calendar_days: u64,
    current_streak_days: u64,
    longest_streak_days: u64,
    longest_session_ms: Option<u64>,
    unreadable_sessions: u64,
    unreported_responses: u64,
    unattributed_responses: u64,
    cache_read_tokens: u128,
    cache_write_tokens: u128,
    cache_reports: u64,
    unknown_cache_writes: u64,
    days: BTreeMap<NaiveDate, DayFacts>,
    models: Vec<StatsModelView>,
    omitted_models: usize,
}

/// Interactive state and immutable facts for the two Stats children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsView {
    tab: StatsViewTab,
    range: StatsDateRange,
    today: NaiveDate,
    facts: StatsFacts,
}

impl StatsView {
    /// Build the UI projection from an authoritative local durable snapshot.
    #[must_use]
    pub fn new(snapshot: &heycode_session::SessionStatsSnapshot, today: NaiveDate) -> Self {
        let days = snapshot
            .days()
            .iter()
            .filter_map(|day| {
                NaiveDate::parse_from_str(day.date(), "%Y-%m-%d")
                    .ok()
                    .map(|date| {
                        (
                            date,
                            DayFacts {
                                sessions: day.sessions(),
                                messages: day
                                    .user_messages()
                                    .saturating_add(day.assistant_messages()),
                                input_tokens: day.reported_input_tokens(),
                                output_tokens: day.reported_output_tokens(),
                            },
                        )
                    })
            })
            .collect();
        const MODEL_LIMIT: usize = 100;
        let model_count = snapshot.models().len();
        let models = snapshot
            .models()
            .iter()
            .take(MODEL_LIMIT)
            .map(|model| StatsModelView {
                provider: model.provider().to_owned(),
                model: model.model().to_owned(),
                responses: model.responses(),
                reported_responses: model.reported_responses(),
                input_tokens: model.reported_input_tokens(),
                output_tokens: model.reported_output_tokens(),
            })
            .collect();
        Self {
            tab: StatsViewTab::Overview,
            range: StatsDateRange::AllTime,
            today,
            facts: StatsFacts {
                used_sessions: snapshot.used_sessions(),
                active_sessions: snapshot.active_used_sessions(),
                archived_sessions: snapshot.archived_used_sessions(),
                calendar_days: snapshot.calendar_days(),
                current_streak_days: snapshot.current_streak_days(),
                longest_streak_days: snapshot.longest_streak_days(),
                longest_session_ms: snapshot.longest_session_span_ms(),
                unreadable_sessions: snapshot.unreadable_sessions(),
                unreported_responses: snapshot.unreported_assistant_messages(),
                unattributed_responses: snapshot.unattributed_assistant_messages(),
                cache_read_tokens: snapshot.cache_read_tokens(),
                cache_write_tokens: snapshot.cache_write_tokens(),
                cache_reports: snapshot.cache_reports(),
                unknown_cache_writes: snapshot.unknown_cache_write_reports(),
                days,
                models,
                omitted_models: model_count.saturating_sub(MODEL_LIMIT),
            },
        }
    }

    /// Active child.
    #[must_use]
    pub const fn tab(&self) -> StatsViewTab {
        self.tab
    }

    /// Select Overview or Models.
    pub const fn select_tab(&mut self, tab: StatsViewTab) {
        self.tab = tab;
    }

    /// Toggle Overview/Models.
    pub const fn toggle_tab(&mut self) {
        self.tab = self.tab.other();
    }

    /// Active Overview date scope.
    #[must_use]
    pub const fn range(&self) -> StatsDateRange {
        self.range
    }

    /// Cycle All time → Last 7 days → Last 30 days.
    pub const fn cycle_range(&mut self) {
        self.range = self.range.next();
    }

    /// Provider/model rows. Their scope is always all local history.
    #[must_use]
    pub fn models(&self) -> &[StatsModelView] {
        &self.facts.models
    }

    /// Additional model routes omitted by the bounded interactive projection.
    #[must_use]
    pub const fn omitted_models(&self) -> usize {
        self.facts.omitted_models
    }

    /// Heatmap for the active date scope, aligned Monday-through-Sunday.
    #[must_use]
    pub fn heatmap(&self) -> StatsHeatmap {
        let weeks = self.range.chart_weeks();
        let sunday_offset =
            6_u64.saturating_sub(u64::from(self.today.weekday().num_days_from_monday()));
        let end = self
            .today
            .checked_add_days(Days::new(sunday_offset))
            .unwrap_or(self.today);
        let start = end
            .checked_sub_days(Days::new(weeks.saturating_mul(7).saturating_sub(1)))
            .unwrap_or(end);
        let max_messages = self
            .facts
            .days
            .iter()
            .filter(|(date, _)| **date >= start && **date <= end)
            .map(|(_, facts)| facts.messages)
            .max()
            .unwrap_or(0);
        let weeks = (0..weeks)
            .map(|week| StatsHeatmapWeek {
                cells: std::array::from_fn(|weekday| {
                    let offset = week
                        .saturating_mul(7)
                        .saturating_add(u64::try_from(weekday).unwrap_or_default());
                    let date = start.checked_add_days(Days::new(offset)).unwrap_or(start);
                    let facts = self.facts.days.get(&date);
                    let messages = facts.map_or(0, |facts| facts.messages);
                    StatsHeatmapCell {
                        date,
                        messages,
                        sessions: facts.map_or(0, |facts| facts.sessions),
                        intensity: intensity(messages, max_messages),
                    }
                }),
            })
            .collect::<Vec<_>>();
        let mut prior_month = None;
        let mut months = Vec::new();
        for (index, week) in weeks.iter().enumerate() {
            let date = week.cells()[0].date();
            let month = date.month();
            if prior_month != Some(month) {
                months.push(StatsMonthMarker {
                    week: index,
                    label: date.format("%b").to_string(),
                });
                prior_month = Some(month);
            }
        }
        StatsHeatmap {
            start,
            end,
            weeks,
            months,
        }
    }

    /// Summary for the active range. Values that cannot be truthfully filtered
    /// from the retained aggregate are `None` or remain marked all-time.
    #[must_use]
    pub fn summary(&self) -> StatsOverviewSummary {
        let start = self.range.days().and_then(|days| {
            self.today
                .checked_sub_days(Days::new(days.saturating_sub(1)))
        });
        let selected = self
            .facts
            .days
            .iter()
            .filter(|(date, _)| start.is_none_or(|start| **date >= start) && **date <= self.today)
            .collect::<Vec<_>>();
        let active_dates = selected
            .iter()
            .filter(|(_, facts)| facts.messages > 0)
            .map(|(date, _)| **date)
            .collect::<BTreeSet<_>>();
        let (computed_current, computed_longest) = streaks(&active_dates, self.today);
        let input = selected.iter().fold(0_u128, |sum, (_, facts)| {
            sum.saturating_add(facts.input_tokens)
        });
        let output = selected.iter().fold(0_u128, |sum, (_, facts)| {
            sum.saturating_add(facts.output_tokens)
        });
        let total_messages = selected
            .iter()
            .fold(0_u64, |sum, (_, facts)| sum.saturating_add(facts.messages));
        let most_active_day = selected
            .iter()
            .filter(|(_, facts)| facts.messages > 0)
            .max_by_key(|(date, facts)| (facts.messages, **date))
            .map(|(date, facts)| (**date, facts.messages));
        let all_time = self.range == StatsDateRange::AllTime;
        let token_lower_bound =
            self.facts.unreadable_sessions > 0 || self.facts.unreported_responses > 0;
        let cache_read_lower_bound = self.facts.unreadable_sessions > 0;
        let cache_write_lower_bound = cache_read_lower_bound || self.facts.unknown_cache_writes > 0;
        StatsOverviewSummary {
            favorite_model: self
                .facts
                .models
                .first()
                .map(|model| format!("{}/{}", model.provider(), model.model())),
            total_tokens: StatsNumber::new(input.saturating_add(output), token_lower_bound),
            input_tokens: StatsNumber::new(input, token_lower_bound),
            output_tokens: StatsNumber::new(output, token_lower_bound),
            // The durable snapshot currently retains cache totals by response,
            // not by UTC date. Keep them visible as all-time evidence rather
            // than pretending that they follow the selected date range.
            cache_read_tokens: StatsNumber::new(
                self.facts.cache_read_tokens,
                cache_read_lower_bound,
            ),
            cache_write_tokens: StatsNumber::new(
                self.facts.cache_write_tokens,
                cache_write_lower_bound,
            ),
            sessions: all_time.then_some(self.facts.used_sessions),
            active_sessions: all_time.then_some(self.facts.active_sessions),
            archived_sessions: all_time.then_some(self.facts.archived_sessions),
            active_days: u64::try_from(active_dates.len()).unwrap_or(u64::MAX),
            calendar_days: self.range.days().unwrap_or(self.facts.calendar_days),
            current_streak_days: if all_time {
                self.facts.current_streak_days
            } else {
                computed_current
            },
            longest_streak_days: if all_time {
                self.facts.longest_streak_days
            } else {
                computed_longest
            },
            most_active_day,
            longest_session_ms: all_time.then_some(self.facts.longest_session_ms).flatten(),
            total_messages,
            unreadable_sessions: self.facts.unreadable_sessions,
            unreported_responses: self.facts.unreported_responses,
            unattributed_responses: self.facts.unattributed_responses,
        }
    }

    /// Stable scope/coverage disclosure for the Overview and Models children.
    #[must_use]
    pub fn scope_note(&self) -> String {
        let coverage = if self.facts.unreadable_sessions == 0 {
            "complete bounded scan".to_owned()
        } else {
            format!(
                "partial; {} unreadable log(s) excluded",
                self.facts.unreadable_sessions
            )
        };
        format!("Local durable sessions · active + archived · UTC · {coverage}")
    }

    /// Whether detailed cache evidence exists at all.
    #[must_use]
    pub const fn has_cache_reports(&self) -> bool {
        self.facts.cache_reports > 0
    }

    /// Plain structured equivalent for screen-reader and text-only clients.
    #[must_use]
    pub fn accessible_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Stats".to_owned(),
            format!("view: {}", self.tab.label()),
            format!("scope: {}", self.scope_note()),
        ];
        match self.tab {
            StatsViewTab::Overview => {
                let summary = self.summary();
                lines.push(format!("range: {}", self.range.label()));
                for week in self.heatmap().weeks() {
                    for cell in week.cells().iter().filter(|cell| cell.messages() > 0) {
                        lines.push(format!(
                            "activity: {} · messages={} · sessions={}",
                            cell.date(),
                            cell.messages(),
                            cell.sessions()
                        ));
                    }
                }
                lines.push(format!(
                    "favorite model: {}",
                    summary.favorite_model().unwrap_or("unavailable")
                ));
                lines.push(format!(
                    "reported tokens: total={} · input={} · output={}",
                    accessible_number(summary.total_tokens()),
                    accessible_number(summary.input_tokens()),
                    accessible_number(summary.output_tokens())
                ));
                lines.push(summary.sessions().map_or_else(
                    || "sessions: unavailable for filtered range".to_owned(),
                    |sessions| {
                        format!(
                            "sessions: total={} · active={} · archived={}",
                            sessions,
                            summary.active_sessions().unwrap_or_default(),
                            summary.archived_sessions().unwrap_or_default()
                        )
                    },
                ));
                lines.push(format!(
                    "days: active={} of {} · current streak={} · longest streak={}",
                    summary.active_days(),
                    summary.calendar_days(),
                    summary.current_streak_days(),
                    summary.longest_streak_days()
                ));
                lines.push(summary.most_active_day().map_or_else(
                    || "most active day: unavailable".to_owned(),
                    |(date, messages)| format!("most active day: {date} · messages={messages}"),
                ));
                lines.push(summary.longest_session_ms().map_or_else(
                    || "longest recorded session span: unavailable for this range".to_owned(),
                    |duration| {
                        format!(
                            "longest recorded session span: {}",
                            compact_duration(duration)
                        )
                    },
                ));
                lines.push(format!(
                    "cache all-time: read={} · write={}",
                    accessible_number(summary.cache_read_tokens()),
                    accessible_number(summary.cache_write_tokens())
                ));
                lines.push(format!(
                    "evidence gaps: unreadable sessions={} · unreported responses={} · unattributed responses={}",
                    summary.unreadable_sessions(),
                    summary.unreported_responses(),
                    summary.unattributed_responses()
                ));
                lines.push(
                    "controls: Left or Right switches Overview and Models; r cycles dates; Up returns to tabs; Escape closes"
                        .to_owned(),
                );
            }
            StatsViewTab::Models => {
                lines.push("model scope: all local history".to_owned());
                if self.models().is_empty() {
                    lines.push("models: no attributable responses".to_owned());
                }
                for model in self.models() {
                    lines.push(format!(
                        "model: {}/{} · responses={} · reported={} · input={} · output={} · usage={}",
                        model.provider(),
                        model.model(),
                        model.responses(),
                        model.reported_responses(),
                        model.input_tokens(),
                        model.output_tokens(),
                        if model.usage_complete() {
                            "complete"
                        } else {
                            "partial"
                        }
                    ));
                }
                if self.omitted_models() > 0 {
                    lines.push(format!(
                        "models: {} additional route(s) omitted",
                        self.omitted_models()
                    ));
                }
                lines.push(
                    "controls: Left or Right switches Overview and Models; Up returns to tabs; Escape closes"
                        .to_owned(),
                );
            }
        }
        lines
    }
}

fn accessible_number(number: StatsNumber) -> String {
    if number.is_lower_bound() {
        format!("at least {}", number.value())
    } else {
        number.value().to_string()
    }
}

fn intensity(messages: u64, maximum: u64) -> u8 {
    if messages == 0 || maximum == 0 {
        0
    } else {
        u8::try_from(messages.saturating_mul(4).div_ceil(maximum))
            .unwrap_or(4)
            .clamp(1, 4)
    }
}

fn streaks(dates: &BTreeSet<NaiveDate>, today: NaiveDate) -> (u64, u64) {
    let mut longest = 0_u64;
    let mut run = 0_u64;
    let mut prior = None;
    for date in dates {
        run = if prior
            .is_some_and(|prior: NaiveDate| prior.checked_add_days(Days::new(1)) == Some(*date))
        {
            run.saturating_add(1)
        } else {
            1
        };
        longest = longest.max(run);
        prior = Some(*date);
    }
    let yesterday = today.checked_sub_days(Days::new(1));
    let mut cursor = if dates.contains(&today) {
        Some(today)
    } else if yesterday.is_some_and(|date| dates.contains(&date)) {
        yesterday
    } else {
        None
    };
    let mut current = 0_u64;
    while let Some(date) = cursor.filter(|date| dates.contains(date)) {
        current = current.saturating_add(1);
        cursor = date.checked_sub_days(Days::new(1));
    }
    (current, longest)
}

/// Compact decimal count used by the source Stats summary (`17.2k`, `2.4m`).
#[must_use]
pub fn compact_number(value: u128) -> String {
    const THOUSAND: u128 = 1_000;
    const MILLION: u128 = 1_000_000;
    const BILLION: u128 = 1_000_000_000;
    let (unit, suffix) = if value >= BILLION {
        (BILLION, "b")
    } else if value >= MILLION {
        (MILLION, "m")
    } else if value >= THOUSAND {
        (THOUSAND, "k")
    } else {
        return value.to_string();
    };
    let tenths = value.saturating_mul(10).saturating_add(unit / 2) / unit;
    if tenths.is_multiple_of(10) {
        format!("{}{suffix}", tenths / 10)
    } else {
        format!("{}.{:01}{suffix}", tenths / 10, tenths % 10)
    }
}

/// Compact source-style duration without implying unmeasured active time.
#[must_use]
pub fn compact_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else if milliseconds < 1_000 {
        format!("{milliseconds}ms")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unwrap_used)] // Fixed, valid dates in this test fixture.
    fn fixture() -> StatsView {
        let today = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let days = [
            ("2026-09-09", 2, 3, 2_400, 600),
            ("2026-09-11", 1, 8, 4_800, 1_200),
            ("2026-09-12", 3, 13, 7_200, 2_100),
        ]
        .into_iter()
        .map(|(date, sessions, messages, input_tokens, output_tokens)| {
            (
                NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
                DayFacts {
                    sessions,
                    messages,
                    input_tokens,
                    output_tokens,
                },
            )
        })
        .collect();
        StatsView {
            tab: StatsViewTab::Overview,
            range: StatsDateRange::AllTime,
            today,
            facts: StatsFacts {
                used_sessions: 6,
                active_sessions: 5,
                archived_sessions: 1,
                calendar_days: 4,
                current_streak_days: 2,
                longest_streak_days: 2,
                longest_session_ms: Some(3_600_000),
                unreadable_sessions: 0,
                unreported_responses: 0,
                unattributed_responses: 0,
                cache_read_tokens: 2_000,
                cache_write_tokens: 750,
                cache_reports: 6,
                unknown_cache_writes: 0,
                days,
                models: vec![StatsModelView {
                    provider: "anthropic".to_owned(),
                    model: "claude-opus-4-6".to_owned(),
                    responses: 6,
                    reported_responses: 6,
                    input_tokens: 14_400,
                    output_tokens: 3_900,
                }],
                omitted_models: 0,
            },
        }
    }

    #[test]
    fn overview_exposes_source_structure_without_losing_local_scope() {
        let view = fixture();
        let summary = view.summary();
        assert_eq!(summary.favorite_model(), Some("anthropic/claude-opus-4-6"));
        assert_eq!(summary.sessions(), Some(6));
        assert_eq!(summary.active_sessions(), Some(5));
        assert_eq!(summary.archived_sessions(), Some(1));
        assert_eq!(summary.total_tokens().value(), 18_300);
        assert_eq!(summary.most_active_day(), Some((view.today, 13)));
        assert_eq!(summary.longest_session_ms(), Some(3_600_000));
        assert_eq!(view.heatmap().weeks().len(), 53);
        assert!(view.scope_note().contains("active + archived"));
    }

    #[test]
    fn range_cycle_filters_only_facts_that_retain_date_identity() {
        let mut view = fixture();
        view.cycle_range();
        assert_eq!(view.range(), StatsDateRange::LastSevenDays);
        let summary = view.summary();
        assert_eq!(summary.sessions(), None);
        assert_eq!(summary.active_days(), 3);
        assert_eq!(summary.calendar_days(), 7);
        assert_eq!(summary.total_messages(), 24);
        assert_eq!(view.heatmap().weeks().len(), 1);
        view.cycle_range();
        assert_eq!(view.range(), StatsDateRange::LastThirtyDays);
        view.cycle_range();
        assert_eq!(view.range(), StatsDateRange::AllTime);
    }

    #[test]
    fn heatmap_and_models_retain_intensity_and_evidence_quality() {
        let mut view = fixture();
        let levels = view
            .heatmap()
            .weeks()
            .iter()
            .flat_map(|week| week.cells())
            .map(StatsHeatmapCell::intensity)
            .collect::<BTreeSet<_>>();
        assert!(levels.contains(&0));
        assert!(levels.contains(&4));
        assert_eq!(view.models()[0].total_tokens(), 18_300);
        assert!(view.models()[0].usage_complete());
        assert_eq!(view.omitted_models(), 0);
        view.toggle_tab();
        assert_eq!(view.tab(), StatsViewTab::Models);
        view.toggle_tab();
        assert_eq!(view.tab(), StatsViewTab::Overview);
    }

    #[test]
    fn incomplete_usage_and_cache_are_explicit_lower_bounds() {
        let mut view = fixture();
        view.facts.unreadable_sessions = 2;
        view.facts.unreported_responses = 1;
        view.facts.unknown_cache_writes = 1;
        let summary = view.summary();
        assert!(summary.total_tokens().is_lower_bound());
        assert!(summary.cache_read_tokens().is_lower_bound());
        assert!(summary.cache_write_tokens().is_lower_bound());
        assert!(view.scope_note().contains("2 unreadable log(s) excluded"));
        assert!(
            view.accessible_lines()
                .iter()
                .any(|line| line.contains("at least"))
        );
    }

    #[test]
    fn compact_formatters_match_the_source_summary_density() {
        assert_eq!(compact_number(999), "999");
        assert_eq!(compact_number(17_200), "17.2k");
        assert_eq!(compact_number(2_000_000), "2m");
        assert_eq!(compact_duration(3_600_000), "1h 0m 0s");
    }
}

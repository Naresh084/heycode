//! TEL01 — usage, latency and route, projected from the durable log alone.
//!
//! `/usage` must work with telemetry disabled, so nothing here observes a live
//! request, keeps a counter, or needs a service. Everything is derived from
//! events that were already written, which means it survives resume and is
//! identical whether or not any telemetry provider exists.
//!
//! The projection stays **neutral**: it reports tokens, instants and the route
//! each turn used, and never prices anything. Pricing lives in `heycode-llm`, and
//! this crate does not import it — a Consumer joins the two. That is the same
//! boundary the request projection keeps by emitting neutral `WireMessage`s.
//!
//! Two honesty rules carry over from the token meter (C11) and the usage facts
//! (P12), because a usage display is read by someone deciding whether to keep
//! going:
//!
//! * **A turn that reported no usage is unknown, not zero.** Summing it as zero
//!   understates the bill and the context pressure.
//! * **A total is only as good as its parts.** If any turn is unreported the
//!   session total is a lower bound, and says so.

use crate::{SessionEvent, SessionEventKind, TurnEndReason};

/// Execution plane that produced one tool usage row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolUsageSource {
    /// heycode executed a client/local model tool.
    Local,
    /// Provider emitted exact call/result identities.
    ProviderExact,
    /// Provider reported only an aggregate request count.
    ProviderAggregate,
}

impl ToolUsageSource {
    /// Stable display identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::ProviderExact => "provider-exact",
            Self::ProviderAggregate => "provider-aggregate",
        }
    }
}

/// Durable session-wide usage for one logical tool/execution plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUsage {
    /// Logical/client tool id.
    pub logical: String,
    /// Execution/evidence plane.
    pub source: ToolUsageSource,
    /// Exact dispatched or provider-reported request count.
    pub requests: u32,
    /// Exact successful settlements when individual outcomes exist.
    pub successes: u32,
    /// Exact error settlements when individual outcomes exist.
    pub errors: u32,
    /// Exact calls with no durable result; aggregate rows use zero because
    /// individual settlement is not represented.
    pub unsettled: u32,
    /// Provider aggregate cost evidence; local/exact rows remain Unknown until
    /// a provider publishes an attributable fee.
    pub cost: heycode_core::ServerToolUsageCost,
}

/// How a turn ended, as the durable log recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The turn closed with this reason.
    Settled(TurnEndReason),
    /// The turn began but never closed — the process died mid-turn, or the log
    /// is being read while the turn is still running.
    Open,
}

/// One turn's projected usage, route and timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnUsage {
    /// Zero-based turn index.
    pub turn: u64,
    /// Tokens the provider reported, summed across the turn's steps. `None`
    /// when no step reported usage — unknown, never zero.
    pub usage: Option<heycode_core::TokenUsage>,
    /// How many of this turn's assistant messages reported usage.
    pub reported_steps: u32,
    /// How many produced no usage figure at all.
    pub unreported_steps: u32,
    /// Provider and model the turn's last request used, when a request header
    /// was recorded. Turns may re-route mid-turn; the last one is what a
    /// consumer prices against.
    pub route: Option<(String, String)>,
    /// Wall-clock milliseconds from `turn/start` to `turn/end`, when settled.
    pub duration_ms: Option<u64>,
    /// Milliseconds from the turn's first request header to its first assistant
    /// message — the closest the durable log gets to time-to-first-response.
    pub first_response_ms: Option<u64>,
    /// How the turn ended.
    pub outcome: TurnOutcome,
}

/// Session-wide usage, projected from the log.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionUsage {
    turns: Vec<TurnUsage>,
    tools: Vec<ToolUsage>,
}

impl SessionUsage {
    /// Per-turn rows in log order.
    #[must_use]
    pub fn turns(&self) -> &[TurnUsage] {
        &self.turns
    }

    /// Tool usage in first-call order, with execution planes kept distinct.
    #[must_use]
    pub fn tools(&self) -> &[ToolUsage] {
        &self.tools
    }

    /// Summed reported tokens across every turn.
    ///
    /// This counts only what providers actually reported. Read it together
    /// with [`Self::is_complete`]: when any step went unreported the sum is a
    /// **lower bound**, not the session total.
    #[must_use]
    pub fn reported_tokens(&self) -> heycode_core::TokenUsage {
        self.turns.iter().filter_map(|turn| turn.usage).fold(
            heycode_core::TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
            },
            |total, usage| heycode_core::TokenUsage {
                prompt_tokens: total.prompt_tokens.saturating_add(usage.prompt_tokens),
                completion_tokens: total
                    .completion_tokens
                    .saturating_add(usage.completion_tokens),
            },
        )
    }

    /// Whether every assistant message in the session reported usage.
    ///
    /// `false` means [`Self::reported_tokens`] is a lower bound.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.turns.iter().all(|turn| turn.unreported_steps == 0)
    }

    /// Assistant messages that reported no usage figure.
    #[must_use]
    pub fn unreported_steps(&self) -> u32 {
        self.turns
            .iter()
            .map(|turn| turn.unreported_steps)
            .fold(0, u32::saturating_add)
    }

    /// Distinct provider/model routes used, in first-seen order.
    ///
    /// A session may span several routes, and each prices differently, so a
    /// consumer must not assume one model priced the whole session.
    #[must_use]
    pub fn routes(&self) -> Vec<(String, String)> {
        let mut seen: Vec<(String, String)> = Vec::new();
        for route in self.turns.iter().filter_map(|turn| turn.route.as_ref()) {
            if !seen.contains(route) {
                seen.push(route.clone());
            }
        }
        seen
    }
}

/// Project usage, route and timing from a durable event slice.
///
/// Reads only what is written, so the result is identical on a live session and
/// on one replayed after restart, and needs no telemetry service.
#[must_use]
pub fn project_usage(events: &[SessionEvent]) -> SessionUsage {
    let mut turns: Vec<TurnUsage> = Vec::new();
    // Index by turn rather than assuming a turn's events are contiguous: an
    // interleaved log must still attribute correctly.
    let mut index: std::collections::BTreeMap<u64, usize> = std::collections::BTreeMap::new();
    let mut first_request_ms: std::collections::BTreeMap<u64, u64> =
        std::collections::BTreeMap::new();
    let mut start_ms: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    let mut tools = Vec::<ToolUsage>::new();
    let mut tool_index = std::collections::BTreeMap::<(ToolUsageSource, String), usize>::new();
    let mut local_calls = std::collections::HashMap::<heycode_core::CallId, usize>::new();
    let mut provider_calls = std::collections::HashMap::<heycode_core::CallId, usize>::new();
    let mut local_results = std::collections::HashSet::<heycode_core::CallId>::new();
    let mut provider_results = std::collections::HashSet::<heycode_core::CallId>::new();
    let mut aggregate_usage = std::collections::HashSet::<(heycode_core::RequestId, String)>::new();

    for event in events {
        match &event.kind {
            SessionEventKind::TurnStart { turn } => {
                let position = turns.len();
                index.entry(*turn).or_insert(position);
                if index[turn] == position {
                    turns.push(TurnUsage {
                        turn: *turn,
                        usage: None,
                        reported_steps: 0,
                        unreported_steps: 0,
                        route: None,
                        duration_ms: None,
                        first_response_ms: None,
                        outcome: TurnOutcome::Open,
                    });
                }
                start_ms.insert(*turn, ms(event.time_ms));
            }
            SessionEventKind::RequestHeader { turn, header, .. } => {
                first_request_ms.entry(*turn).or_insert(ms(event.time_ms));
                if let Some(row) = index.get(turn).and_then(|at| turns.get_mut(*at)) {
                    // Last route wins: a turn that re-routed mid-flight is
                    // priced against what it actually last used.
                    row.route = Some((header.provider.clone(), header.model.clone()));
                }
            }
            SessionEventKind::AssistantMessage { turn, usage, .. } => {
                let Some(row) = index.get(turn).and_then(|at| turns.get_mut(*at)) else {
                    continue;
                };
                match usage {
                    Some(reported) => {
                        row.reported_steps = row.reported_steps.saturating_add(1);
                        let total = row.usage.unwrap_or(heycode_core::TokenUsage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                        });
                        row.usage = Some(heycode_core::TokenUsage {
                            prompt_tokens: total
                                .prompt_tokens
                                .saturating_add(reported.prompt_tokens),
                            completion_tokens: total
                                .completion_tokens
                                .saturating_add(reported.completion_tokens),
                        });
                    }
                    // A step with no usage figure leaves the turn's total
                    // unknown-by-that-much rather than silently adding zero.
                    None => row.unreported_steps = row.unreported_steps.saturating_add(1),
                }
                if row.first_response_ms.is_none()
                    && let Some(requested) = first_request_ms.get(turn)
                {
                    row.first_response_ms = ms(event.time_ms).checked_sub(*requested);
                }
            }
            SessionEventKind::TurnEnd { turn, reason } => {
                let Some(row) = index.get(turn).and_then(|at| turns.get_mut(*at)) else {
                    continue;
                };
                row.outcome = TurnOutcome::Settled(*reason);
                if let Some(started) = start_ms.get(turn) {
                    // A clock that went backwards yields no duration rather
                    // than a wrapped one.
                    row.duration_ms = ms(event.time_ms).checked_sub(*started);
                }
            }
            SessionEventKind::ToolCall { call_id, name, .. } => {
                if local_calls.contains_key(call_id) {
                    continue;
                }
                let row = tool_row(&mut tools, &mut tool_index, ToolUsageSource::Local, name);
                tools[row].requests = tools[row].requests.saturating_add(1);
                local_calls.entry(call_id.clone()).or_insert(row);
            }
            SessionEventKind::ToolResult {
                call_id, is_error, ..
            }
            | SessionEventKind::RichToolResult {
                call_id, is_error, ..
            } => {
                if local_results.insert(call_id.clone())
                    && let Some(row) = local_calls.get(call_id).copied()
                {
                    settle_tool(&mut tools[row], *is_error);
                }
            }
            SessionEventKind::ServerToolCall { call, .. } => {
                if provider_calls.contains_key(call.id()) {
                    continue;
                }
                let row = tool_row(
                    &mut tools,
                    &mut tool_index,
                    ToolUsageSource::ProviderExact,
                    call.logical(),
                );
                tools[row].requests = tools[row].requests.saturating_add(1);
                provider_calls.entry(call.id().clone()).or_insert(row);
            }
            SessionEventKind::ServerToolResult { result, .. } => {
                if provider_results.insert(result.call_id().clone())
                    && let Some(row) = provider_calls.get(result.call_id()).copied()
                {
                    settle_tool(
                        &mut tools[row],
                        result.outcome() == heycode_core::ServerToolOutcome::Error,
                    );
                }
            }
            SessionEventKind::ServerToolUsage {
                request_id, usage, ..
            } => {
                if !aggregate_usage.insert((request_id.clone(), usage.logical().to_owned())) {
                    continue;
                }
                let row = tool_row(
                    &mut tools,
                    &mut tool_index,
                    ToolUsageSource::ProviderAggregate,
                    usage.logical(),
                );
                let first = tools[row].requests == 0;
                tools[row].requests = tools[row].requests.saturating_add(usage.requests());
                if first {
                    tools[row].cost = usage.cost().clone();
                } else if tools[row].cost != *usage.cost() {
                    tools[row].cost = heycode_core::ServerToolUsageCost::Unknown;
                }
            }
            _ => {}
        }
    }
    for row in &mut tools {
        if row.source != ToolUsageSource::ProviderAggregate {
            row.unsettled = row
                .requests
                .saturating_sub(row.successes.saturating_add(row.errors));
        }
    }
    SessionUsage { turns, tools }
}

fn tool_row(
    rows: &mut Vec<ToolUsage>,
    index: &mut std::collections::BTreeMap<(ToolUsageSource, String), usize>,
    source: ToolUsageSource,
    logical: &str,
) -> usize {
    let key = (source, logical.to_owned());
    if let Some(row) = index.get(&key) {
        return *row;
    }
    let row = rows.len();
    rows.push(ToolUsage {
        logical: logical.to_owned(),
        source,
        requests: 0,
        successes: 0,
        errors: 0,
        unsettled: 0,
        cost: heycode_core::ServerToolUsageCost::Unknown,
    });
    index.insert(key, row);
    row
}

fn settle_tool(row: &mut ToolUsage, is_error: bool) {
    if is_error {
        row.errors = row.errors.saturating_add(1);
    } else {
        row.successes = row.successes.saturating_add(1);
    }
}

/// Clamp a signed log instant to milliseconds, treating pre-epoch as zero.
const fn ms(time_ms: i64) -> u64 {
    if time_ms < 0 { 0 } else { time_ms as u64 }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn event(seq: u64, time_ms: i64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            v: crate::CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms,
            kind,
        }
    }

    fn usage(prompt: u64, completion: u64) -> heycode_core::TokenUsage {
        heycode_core::TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
        }
    }

    fn assistant(
        turn: u64,
        step: u32,
        reported: Option<heycode_core::TokenUsage>,
    ) -> SessionEventKind {
        SessionEventKind::AssistantMessage {
            turn,
            step,
            content: "ok".to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: reported,
        }
    }

    #[test]
    fn a_turn_sums_its_reported_steps_and_records_its_timing() {
        let events = vec![
            event(0, 1_000, SessionEventKind::TurnStart { turn: 0 }),
            event(1, 1_200, assistant(0, 1, Some(usage(10, 5)))),
            event(2, 1_500, assistant(0, 2, Some(usage(20, 7)))),
            event(
                3,
                2_000,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        let projected = project_usage(&events);
        let turn = &projected.turns()[0];
        assert_eq!(turn.usage, Some(usage(30, 12)), "steps sum within a turn");
        assert_eq!(turn.reported_steps, 2);
        assert_eq!(turn.unreported_steps, 0);
        assert_eq!(turn.duration_ms, Some(1_000));
        assert_eq!(turn.outcome, TurnOutcome::Settled(TurnEndReason::Stop));
        assert!(projected.is_complete());
        assert_eq!(projected.reported_tokens(), usage(30, 12));
    }

    #[test]
    fn an_unreported_step_makes_the_total_a_lower_bound_and_is_never_summed_as_zero() {
        let events = vec![
            event(0, 1_000, SessionEventKind::TurnStart { turn: 0 }),
            event(1, 1_100, assistant(0, 1, Some(usage(10, 5)))),
            event(2, 1_200, assistant(0, 2, None)),
            event(
                3,
                1_300,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        let projected = project_usage(&events);
        let turn = &projected.turns()[0];
        // The reported step still counts; the silent one is named, not zeroed.
        assert_eq!(turn.usage, Some(usage(10, 5)));
        assert_eq!(turn.unreported_steps, 1);
        assert!(
            !projected.is_complete(),
            "an unreported step means the session total is a lower bound"
        );
        assert_eq!(projected.unreported_steps(), 1);
    }

    #[test]
    fn a_turn_that_reported_nothing_is_unknown_rather_than_zero() {
        let events = vec![
            event(0, 1_000, SessionEventKind::TurnStart { turn: 0 }),
            event(1, 1_100, assistant(0, 1, None)),
            event(
                2,
                1_200,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Error,
                },
            ),
        ];
        let projected = project_usage(&events);
        assert_eq!(
            projected.turns()[0].usage,
            None,
            "no reported usage is unknown, and unknown has no number"
        );
        assert!(!projected.is_complete());
    }

    #[test]
    fn an_open_turn_is_distinguishable_from_a_settled_one_and_has_no_duration() {
        // A process that died mid-turn leaves `turn/start` with no `turn/end`.
        let events = vec![
            event(0, 1_000, SessionEventKind::TurnStart { turn: 0 }),
            event(1, 1_100, assistant(0, 1, Some(usage(3, 1)))),
        ];
        let projected = project_usage(&events);
        let turn = &projected.turns()[0];
        assert_eq!(turn.outcome, TurnOutcome::Open);
        assert_eq!(turn.duration_ms, None, "an unsettled turn has no duration");
        assert_eq!(
            turn.usage,
            Some(usage(3, 1)),
            "its reported work still counts"
        );
    }

    #[test]
    fn a_backwards_clock_yields_no_duration_rather_than_a_wrapped_one() {
        let events = vec![
            event(0, 5_000, SessionEventKind::TurnStart { turn: 0 }),
            event(
                1,
                1_000,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
        ];
        assert_eq!(project_usage(&events).turns()[0].duration_ms, None);
    }

    #[test]
    fn the_projection_needs_no_service_and_survives_replay_of_an_interleaved_log() {
        // Turn events need not be contiguous; attribution is by turn index.
        let events = vec![
            event(0, 1_000, SessionEventKind::TurnStart { turn: 0 }),
            event(1, 1_050, SessionEventKind::TurnStart { turn: 1 }),
            event(2, 1_100, assistant(1, 1, Some(usage(1, 1)))),
            event(3, 1_150, assistant(0, 1, Some(usage(2, 2)))),
            event(
                4,
                1_200,
                SessionEventKind::TurnEnd {
                    turn: 1,
                    reason: TurnEndReason::Stop,
                },
            ),
            event(
                5,
                1_400,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Aborted,
                },
            ),
        ];
        let projected = project_usage(&events);
        assert_eq!(projected.turns().len(), 2);
        assert_eq!(projected.turns()[0].turn, 0);
        assert_eq!(projected.turns()[0].usage, Some(usage(2, 2)));
        assert_eq!(projected.turns()[0].duration_ms, Some(400));
        assert_eq!(projected.turns()[1].turn, 1);
        assert_eq!(projected.turns()[1].usage, Some(usage(1, 1)));
        assert_eq!(projected.turns()[1].duration_ms, Some(150));

        // Projecting the same slice twice is identical: no counter, no state.
        assert_eq!(project_usage(&events), projected);
    }

    #[test]
    fn an_empty_log_reports_nothing_and_is_trivially_complete() {
        let projected = project_usage(&[]);
        assert!(projected.turns().is_empty());
        assert_eq!(projected.reported_tokens(), usage(0, 0));
        assert!(
            projected.is_complete(),
            "nothing to report is complete; it is unreported steps that make a bound"
        );
        assert!(projected.routes().is_empty());
    }

    #[test]
    fn local_exact_provider_and_aggregate_provider_tools_stay_distinct() {
        let local_id = heycode_core::CallId::from_raw("call_local");
        let server_id = heycode_core::CallId::from_raw("call_server");
        let request_id = heycode_core::RequestId::from_raw("request_server");
        let events = vec![
            event(0, 1, SessionEventKind::TurnStart { turn: 1 }),
            event(
                1,
                2,
                SessionEventKind::ToolCall {
                    turn: 1,
                    call_id: local_id.clone(),
                    name: "read".to_owned(),
                    args: serde_json::json!({"path":"private"}),
                },
            ),
            event(
                2,
                3,
                SessionEventKind::ToolResult {
                    call_id: local_id,
                    content: "private output".to_owned(),
                    is_error: false,
                    untrusted_content: None,
                },
            ),
            event(
                3,
                4,
                SessionEventKind::ServerToolCall {
                    turn: 1,
                    step: 1,
                    request_id: request_id.clone(),
                    output_index: 0,
                    call: Box::new(
                        heycode_core::ServerToolCall::new(
                            server_id.clone(),
                            "web_search",
                            "web_search",
                            serde_json::json!({"query":"private"}),
                        )
                        .unwrap(),
                    ),
                },
            ),
            event(
                4,
                5,
                SessionEventKind::ServerToolResult {
                    turn: 1,
                    step: 1,
                    request_id: request_id.clone(),
                    output_index: 0,
                    result: Box::new(
                        heycode_core::ServerToolResult::error(server_id, "provider_error").unwrap(),
                    ),
                },
            ),
            event(
                5,
                6,
                SessionEventKind::ServerToolUsage {
                    turn: 1,
                    step: 1,
                    request_id,
                    usage: Box::new(
                        heycode_core::ServerToolUsage::new(
                            "web_search",
                            2,
                            heycode_core::ServerToolUsageEvidence::ProviderAggregate,
                            heycode_core::ServerToolUsageCost::Unknown,
                        )
                        .unwrap(),
                    ),
                },
            ),
        ];

        let tools = project_usage(&events).tools().to_vec();
        assert_eq!(tools.len(), 3);
        assert!(tools.iter().any(|row| {
            row.source == ToolUsageSource::Local
                && row.logical == "read"
                && row.requests == 1
                && row.successes == 1
                && row.errors == 0
        }));
        assert!(tools.iter().any(|row| {
            row.source == ToolUsageSource::ProviderExact
                && row.logical == "web_search"
                && row.requests == 1
                && row.errors == 1
        }));
        assert!(tools.iter().any(|row| {
            row.source == ToolUsageSource::ProviderAggregate
                && row.logical == "web_search"
                && row.requests == 2
                && row.cost == heycode_core::ServerToolUsageCost::Unknown
        }));
    }
}

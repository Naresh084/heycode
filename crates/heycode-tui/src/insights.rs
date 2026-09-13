//! Read-only structural insights over the bounded local session store.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandDescriptor, CommandMetadataError, CommandSource, CommandTiming, UiEvent,
};
use heycode_session::{
    SessionEvent, SessionEventKind, SessionFilter, SessionQuery, SessionQueryError,
    SessionQueryService, SessionStorageFilter, ToolUsageSource, TurnEndReason,
};

const MAX_ANALYZED_SESSIONS: usize = 100;
const TOP_ROWS: usize = 5;

/// Build the TUI-owned `/insights` command.
///
/// # Errors
/// Invalid static command metadata fails plugin composition.
pub fn command(
    source: CommandSource,
    sessions: Arc<SessionQueryService>,
) -> Result<Arc<dyn Command>, CommandMetadataError> {
    Ok(Arc::new(InsightsCommand {
        descriptor: CommandDescriptor::new(
            "insights",
            "Summarize structural facts from local durable session journals",
            Vec::new(),
            CommandTiming::Queued,
            source,
        )?,
        sessions,
    }))
}

struct InsightsCommand {
    descriptor: CommandDescriptor,
    sessions: Arc<SessionQueryService>,
}

#[async_trait]
impl Command for InsightsCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        if !args.trim().is_empty() {
            anyhow::bail!("usage: /insights");
        }
        let sessions = self.sessions.clone();
        let report = tokio::task::spawn_blocking(move || render(&sessions))
            .await
            .map_err(|error| anyhow::anyhow!("local insights task failed: {error}"))??;
        agent.ui().emit(UiEvent::Info { text: report });
        Ok(())
    }
}

#[derive(Default)]
struct Insights {
    selected_sessions: u64,
    analyzed_sessions: u64,
    unavailable_sessions: u64,
    archived_sessions: u64,
    fork_sessions: u64,
    local_events: u64,
    user_messages: u64,
    assistant_messages: u64,
    reported_steps: u64,
    unreported_steps: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    turns: u64,
    outcomes: BTreeMap<&'static str, u64>,
    routes: BTreeMap<String, u64>,
    project_areas: BTreeMap<String, u64>,
    tools: [ToolPlane; 3],
    tool_names: BTreeMap<String, u64>,
}

#[derive(Clone, Copy, Default)]
struct ToolPlane {
    requests: u64,
    successes: u64,
    errors: u64,
    unsettled: u64,
}

fn render(sessions: &SessionQueryService) -> Result<String, SessionQueryError> {
    let filter = SessionFilter::new()
        .with_used_only()
        .with_storage(SessionStorageFilter::All);
    let page = sessions.query(&SessionQuery::new(filter, MAX_ANALYZED_SESSIONS)?)?;
    let total_matches = page.total_matches();
    let mut insights = Insights::default();

    for summary in page.items() {
        insights.selected_sessions = insights.selected_sessions.saturating_add(1);
        if summary.storage() == heycode_session::SessionStorageState::Archived {
            insights.archived_sessions = insights.archived_sessions.saturating_add(1);
        }
        if summary.lineage().is_some() {
            insights.fork_sessions = insights.fork_sessions.saturating_add(1);
        }
        let project = summary.cwd().map_or_else(
            || "unknown cwd".to_owned(),
            |cwd| sanitize_label(&cwd.display().to_string()),
        );
        increment(&mut insights.project_areas, project, 1);

        if !summary.is_readable() {
            insights.unavailable_sessions = insights.unavailable_sessions.saturating_add(1);
            continue;
        }
        let session = match sessions.resume(summary.id()) {
            Ok(session) => session,
            // A concurrent lifecycle operation can invalidate a row between
            // listing and opening. Keep the report useful and name the gap.
            Err(_) => {
                insights.unavailable_sessions = insights.unavailable_sessions.saturating_add(1);
                continue;
            }
        };
        let first_local = match usize::try_from(session.first_local_seq()) {
            Ok(first_local) => first_local,
            Err(_) => {
                insights.unavailable_sessions = insights.unavailable_sessions.saturating_add(1);
                continue;
            }
        };
        let Some(events) = session.events().get(first_local..) else {
            insights.unavailable_sessions = insights.unavailable_sessions.saturating_add(1);
            continue;
        };
        insights.analyzed_sessions = insights.analyzed_sessions.saturating_add(1);
        insights.observe(events);
    }

    Ok(insights.report(total_matches))
}

impl Insights {
    fn observe(&mut self, events: &[SessionEvent]) {
        self.local_events = self
            .local_events
            .saturating_add(u64::try_from(events.len()).unwrap_or(u64::MAX));
        let mut started = BTreeSet::new();
        let mut ended = BTreeSet::new();
        let mut session_routes = BTreeMap::<u64, String>::new();

        for event in events {
            match &event.kind {
                SessionEventKind::TurnStart { turn } => {
                    started.insert(*turn);
                }
                SessionEventKind::TurnEnd { turn, reason } => {
                    ended.insert(*turn);
                    increment(&mut self.outcomes, outcome_name(*reason), 1);
                }
                SessionEventKind::RequestHeader { turn, header, .. } => {
                    session_routes.insert(
                        *turn,
                        sanitize_label(&format!("{}/{}", header.provider, header.model)),
                    );
                }
                SessionEventKind::UserMessage { .. } => {
                    self.user_messages = self.user_messages.saturating_add(1);
                }
                SessionEventKind::AssistantMessage { usage, .. } => {
                    self.assistant_messages = self.assistant_messages.saturating_add(1);
                    if let Some(usage) = usage {
                        self.reported_steps = self.reported_steps.saturating_add(1);
                        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
                        self.completion_tokens = self
                            .completion_tokens
                            .saturating_add(usage.completion_tokens);
                    } else {
                        self.unreported_steps = self.unreported_steps.saturating_add(1);
                    }
                }
                _ => {}
            }
        }

        let turns = started.union(&ended).count();
        self.turns = self
            .turns
            .saturating_add(u64::try_from(turns).unwrap_or(u64::MAX));
        let open = started.difference(&ended).count();
        if open != 0 {
            increment(
                &mut self.outcomes,
                "open",
                u64::try_from(open).unwrap_or(u64::MAX),
            );
        }
        for route in session_routes.into_values() {
            increment(&mut self.routes, route, 1);
        }

        for tool in heycode_session::project_usage(events).tools() {
            let plane = match tool.source {
                ToolUsageSource::Local => 0,
                ToolUsageSource::ProviderExact => 1,
                ToolUsageSource::ProviderAggregate => 2,
            };
            let target = &mut self.tools[plane];
            target.requests = target.requests.saturating_add(u64::from(tool.requests));
            target.successes = target.successes.saturating_add(u64::from(tool.successes));
            target.errors = target.errors.saturating_add(u64::from(tool.errors));
            target.unsettled = target.unsettled.saturating_add(u64::from(tool.unsettled));
            increment(
                &mut self.tool_names,
                format!("{}:{}", tool.source.as_str(), sanitize_label(&tool.logical)),
                u64::from(tool.requests),
            );
        }
    }

    fn report(&self, total_matches: usize) -> String {
        let total_matches = u64::try_from(total_matches).unwrap_or(u64::MAX);
        let selection = if self.selected_sessions < total_matches {
            format!(
                "newest {} of {total_matches} used sessions",
                self.selected_sessions
            )
        } else {
            format!("all {total_matches} used sessions")
        };
        let mut lines = vec![
            "insights".to_owned(),
            format!(
                "scope: {selection} in this configured local store; active + archived; maximum {MAX_ANALYZED_SESSIONS} analyzed"
            ),
            "privacy: report aggregates structural journal fields only; no semantic inference, message text output, model call, network request, or upload".to_owned(),
            format!(
                "sessions: {} analyzed · {} unavailable · {} archived · {} forks",
                self.analyzed_sessions,
                self.unavailable_sessions,
                self.archived_sessions,
                self.fork_sessions
            ),
            format!(
                "activity: {} local events · {} turns · {} user messages · {} assistant messages",
                self.local_events, self.turns, self.user_messages, self.assistant_messages
            ),
            format!("turn outcomes: {}", render_counts(&self.outcomes, 10)),
            self.usage_line(),
            format!("local tools: {}", render_tool_plane(self.tools[0], true)),
            format!(
                "provider-exact tools: {}",
                render_tool_plane(self.tools[1], true)
            ),
            format!(
                "provider-aggregate tools: {}",
                render_tool_plane(self.tools[2], false)
            ),
            format!("top tool rows: {}", render_counts(&self.tool_names, TOP_ROWS)),
            format!("routes by turn: {}", render_counts(&self.routes, TOP_ROWS)),
            format!(
                "project areas by creation cwd: {}",
                render_counts(&self.project_areas, TOP_ROWS)
            ),
        ];
        lines.push(self.friction_line());
        lines.push(
            "counting: physical local journal suffixes only, so inherited fork prefixes are not counted twice"
                .to_owned(),
        );
        lines.join("\n")
    }

    fn usage_line(&self) -> String {
        if self.reported_steps == 0 && self.unreported_steps != 0 {
            return format!(
                "usage: unknown · {} assistant steps reported no token usage",
                self.unreported_steps
            );
        }
        let bound = if self.unreported_steps == 0 {
            "exact for analyzed assistant steps"
        } else {
            "at least; some assistant steps are unknown"
        };
        format!(
            "usage: {} prompt + {} completion tokens reported ({bound}) · {} reported steps · {} unreported steps",
            self.prompt_tokens, self.completion_tokens, self.reported_steps, self.unreported_steps
        )
    }

    fn friction_line(&self) -> String {
        let errors = self.outcomes.get("error").copied().unwrap_or_default();
        let aborted = self.outcomes.get("aborted").copied().unwrap_or_default();
        let limits = [
            "max tokens",
            "max steps",
            "max elapsed",
            "max tool calls",
            "unreported token usage",
            "clock unavailable",
        ]
        .into_iter()
        .filter_map(|name| self.outcomes.get(name))
        .copied()
        .fold(0_u64, u64::saturating_add);
        let open = self.outcomes.get("open").copied().unwrap_or_default();
        let tool_errors = self.tools[0].errors.saturating_add(self.tools[1].errors);
        let unsettled = self.tools[0]
            .unsettled
            .saturating_add(self.tools[1].unsettled);
        format!(
            "durable friction signals: {errors} error-ended turns · {aborted} aborted · {limits} policy/limit ends · {open} open · {tool_errors} exact tool errors · {unsettled} exact tool calls without results"
        )
    }
}

fn render_tool_plane(row: ToolPlane, outcomes_known: bool) -> String {
    if outcomes_known {
        format!(
            "{} requests · {} succeeded · {} errors · {} without results",
            row.requests, row.successes, row.errors, row.unsettled
        )
    } else {
        format!("{} requests · individual outcomes unknown", row.requests)
    }
}

fn render_counts<K>(rows: &BTreeMap<K, u64>, limit: usize) -> String
where
    K: Ord + AsRef<str>,
{
    let mut rows = rows.iter().collect::<Vec<_>>();
    rows.sort_by(|(left_key, left_count), (right_key, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_key.as_ref().cmp(right_key.as_ref()))
    });
    let rendered = rows
        .into_iter()
        .take(limit)
        .map(|(key, count)| format!("{}={count}", key.as_ref()))
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        "none".to_owned()
    } else {
        rendered.join(" · ")
    }
}

fn increment<K>(rows: &mut BTreeMap<K, u64>, key: K, value: u64)
where
    K: Ord,
{
    let current = rows.entry(key).or_default();
    *current = current.saturating_add(value);
}

const fn outcome_name(reason: TurnEndReason) -> &'static str {
    match reason {
        TurnEndReason::Stop => "stop",
        TurnEndReason::MaxTokens => "max tokens",
        TurnEndReason::MaxSteps => "max steps",
        TurnEndReason::MaxElapsed => "max elapsed",
        TurnEndReason::MaxToolCalls => "max tool calls",
        TurnEndReason::UnreportedTokenUsage => "unreported token usage",
        TurnEndReason::ClockUnavailable => "clock unavailable",
        TurnEndReason::Error => "error",
        TurnEndReason::Aborted => "aborted",
    }
}

fn sanitize_label(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars().take(120) {
        if character.is_control() {
            result.push('�');
        } else if character == '`' {
            result.push('\'');
        } else {
            result.push(character);
        }
    }
    if value.chars().count() > 120 {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use heycode_session::{ForkBoundary, Session};

    fn append_turn(
        session: &mut Session,
        turn: u64,
        usage: Option<heycode_core::TokenUsage>,
        reason: TurnEndReason,
    ) {
        session
            .append(SessionEventKind::TurnStart { turn })
            .unwrap();
        session
            .append(SessionEventKind::UserMessage {
                text: "private prompt text".to_owned(),
            })
            .unwrap();
        session
            .append(SessionEventKind::AssistantMessage {
                turn,
                step: 0,
                content: "private response text".to_owned(),
                reasoning: None,
                tool_calls: None,
                usage,
            })
            .unwrap();
        session
            .append(SessionEventKind::TurnEnd { turn, reason })
            .unwrap();
    }

    #[test]
    fn command_is_discoverable_queued_and_argument_free() {
        let root = tempfile::tempdir().unwrap();
        let command = command(
            CommandSource::from_plugin("tui").unwrap(),
            Arc::new(SessionQueryService::local(root.path().to_path_buf())),
        )
        .unwrap();
        assert_eq!(command.descriptor().id(), "insights");
        assert_eq!(command.descriptor().timing(), CommandTiming::Queued);
        assert!(command.descriptor().arguments().is_empty());
    }

    #[test]
    fn report_uses_local_suffixes_and_keeps_missing_usage_unknown() {
        let root = tempfile::tempdir().unwrap();
        let mut parent = Session::create(root.path()).unwrap();
        append_turn(
            &mut parent,
            0,
            Some(heycode_core::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 4,
            }),
            TurnEndReason::Stop,
        );
        let mut child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
        append_turn(&mut child, 1, None, TurnEndReason::Error);
        drop(child);
        drop(parent);

        let report = render(&SessionQueryService::local(root.path().to_path_buf())).unwrap();
        assert!(report.contains("all 2 used sessions"));
        assert!(report.contains("2 analyzed · 0 unavailable · 0 archived · 1 forks"));
        assert!(report.contains("2 turns · 2 user messages · 2 assistant messages"));
        assert!(report.contains("stop=1"));
        assert!(report.contains("error=1"));
        assert!(report.contains("usage: 10 prompt + 4 completion tokens reported (at least"));
        assert!(report.contains("1 reported steps · 1 unreported steps"));
        assert!(!report.contains("private prompt text"));
        assert!(!report.contains("private response text"));
    }
}

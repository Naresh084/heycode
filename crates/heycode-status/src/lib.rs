//! Effective product status and diagnostics commands.
//!
//! The command plugin reads already-composed services. It never reconstructs
//! desired state from config and never publishes a sandbox change before a
//! durable hot-reload owner exists.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::ui::{SettingsShellSection, SettingsShellSnapshot, SettingsShellTab};
use heycode_agent::{Command, CommandDescriptor, CommandSource, CommandTiming, UiEvent};
use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_doctor::{DoctorRegistry, DoctorReport};
use heycode_exec::{NetworkScope, SandboxCapabilityReport, SandboxService};
use tokio_util::sync::CancellationToken;

pub mod health;

const COMMAND_IDS: [&str; 5] = ["status", "doctor", "permissions", "sandbox", "config"];
const CONTEXT_COMMAND_IDS: [&str; 3] = ["context", "usage", "stats"];

/// Shared immutable snapshot authority for the settings/status shell.
pub const SERVICE_SETTINGS_SHELL: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("settings-shell-snapshot");

/// Mount `/status`, `/doctor`, `/permissions`, `/sandbox` and `/config` as
/// effect-owned human-plane commands. Bare `/config` requests the settings
/// owner; without a report, explicit `/config show` says so. The composition
/// root passes a report through [`status_plugin_with_config`].
#[must_use]
pub fn status_plugin() -> Box<dyn Plugin> {
    status_plugin_with(None)
}

/// [`status_plugin`] whose `/config show` renders the effective configuration
/// with the file or flag each value came from.
#[must_use]
pub fn status_plugin_with_config(report: heycode_config::ConfigReport) -> Box<dyn Plugin> {
    status_plugin_with(Some(report))
}

fn status_plugin_with(config: Option<heycode_config::ConfigReport>) -> Box<dyn Plugin> {
    struct StatusPlugin {
        config: Option<heycode_config::ConfigReport>,
    }

    impl Plugin for StatusPlugin {
        fn name(&self) -> &'static str {
            "status"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            COMMAND_IDS
                .into_iter()
                .map(|id| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::Command,
                        id,
                    )
                })
                .collect()
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SETTINGS_SHELL]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_agent::SERVICE_COMMANDS,
                heycode_doctor::SERVICE_DOCTOR,
                heycode_exec::SERVICE_SANDBOX,
                heycode_prompt::SERVICE_PROMPT,
                heycode_llm::SERVICE_MODELS,
                heycode_session::SERVICE_SESSION_QUERY,
                // Orders this plugin after the approval plugin, so the optional
                // `approval-switch` lookup below sees a surface that has one.
                heycode_agent::SERVICE_APPROVAL,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            let doctor = get::<DoctorRegistry>(context, heycode_doctor::SERVICE_DOCTOR)?;
            let sandbox = get::<SandboxService>(context, heycode_exec::SERVICE_SANDBOX)?;
            let prompt =
                get::<heycode_prompt::PromptRegistry>(context, heycode_prompt::SERVICE_PROMPT)?;
            let models = get::<heycode_llm::CatalogRegistry>(context, heycode_llm::SERVICE_MODELS)?;
            let sessions = get::<heycode_session::SessionQueryService>(
                context,
                heycode_session::SERVICE_SESSION_QUERY,
            )?;
            // Present only on surfaces that can prompt; its absence means
            // `/permissions <mode>` reports that the mode is fixed here.
            let approval_switch = context
                .get::<heycode_agent::ApprovalSwitchHandle>(heycode_agent::SERVICE_APPROVAL_SWITCH)
                .map(|handle| handle.0.clone());
            let lifecycle = CancellationToken::new();
            let shutdown = lifecycle.clone();
            context.effect(move || shutdown.cancel());
            let state = Arc::new(StatusState {
                doctor,
                sandbox,
                prompt,
                approval_switch,
                lifecycle,
                config: self.config.clone(),
            });
            context.provide(
                SERVICE_SETTINGS_SHELL,
                self.name(),
                SettingsShellSnapshotService {
                    status: state.clone(),
                    models,
                    sessions,
                },
            )?;
            let shell = get::<SettingsShellSnapshotService>(context, SERVICE_SETTINGS_SHELL)?;
            let source = CommandSource::from_plugin(self.name()).map_err(core_error)?;
            for command in commands_for(state, shell, source).map_err(core_error)? {
                commands
                    .register_effect(context, command)
                    .map_err(core_error)?;
            }
            Ok(())
        }
    }

    Box::new(StatusPlugin { config })
}

/// Mount `/context` and `/usage` over the live Agent envelope, durable session
/// projection and last-good model pricing.
#[must_use]
pub fn context_status_plugin() -> Box<dyn Plugin> {
    struct ContextStatusPlugin;

    impl Plugin for ContextStatusPlugin {
        fn name(&self) -> &'static str {
            "status-context"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Command],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            CONTEXT_COMMAND_IDS
                .into_iter()
                .map(|id| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::Command,
                        id,
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_agent::SERVICE_COMMANDS,
                heycode_llm::SERVICE_MODELS,
                SERVICE_SETTINGS_SHELL,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            let models = get::<heycode_llm::CatalogRegistry>(context, heycode_llm::SERVICE_MODELS)?;
            let shell = get::<SettingsShellSnapshotService>(context, SERVICE_SETTINGS_SHELL)?;
            let source = CommandSource::from_plugin(self.name()).map_err(core_error)?;
            for command in context_commands(models, shell, source).map_err(core_error)? {
                commands
                    .register_effect(context, command)
                    .map_err(core_error)?;
            }
            Ok(())
        }
    }

    Box::new(ContextStatusPlugin)
}

/// Mount `/web` only when the replaceable web capability is composed.
#[must_use]
pub fn web_status_plugin() -> Box<dyn Plugin> {
    struct WebStatusPlugin;

    impl Plugin for WebStatusPlugin {
        fn name(&self) -> &'static str {
            "status-web"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Command],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Command,
                "web",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_agent::SERVICE_COMMANDS, heycode_web::SERVICE_WEB]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            let web = get::<heycode_web::WebRegistry>(context, heycode_web::SERVICE_WEB)?;
            let source = CommandSource::from_plugin(self.name()).map_err(core_error)?;
            let command = Arc::new(WebStatusCommand {
                descriptor: descriptor(
                    "web",
                    "Show registered web providers, selections and domain policy",
                    source,
                )
                .map_err(core_error)?,
                web,
            });
            commands
                .register_effect(context, command)
                .map_err(core_error)
        }
    }

    Box::new(WebStatusPlugin)
}

pub(crate) fn get<T: Send + Sync + 'static>(
    context: &Context,
    key: heycode_core::ServiceKey,
) -> CoreResult<Arc<T>> {
    context
        .get::<T>(key)
        .ok_or_else(|| CoreError::MissingService(key.to_string()))
}

pub(crate) fn core_error(error: impl ToString) -> CoreError {
    CoreError::other(error.to_string())
}

struct StatusState {
    doctor: Arc<DoctorRegistry>,
    sandbox: Arc<SandboxService>,
    prompt: Arc<heycode_prompt::PromptRegistry>,
    approval_switch: Option<Arc<heycode_agent::SwitchableApproval>>,
    lifecycle: CancellationToken,
    config: Option<heycode_config::ConfigReport>,
}

/// Authoritative snapshot requester shared by canonical settings-shell commands.
///
/// Front ends may use [`Self::request_config`] to make compatibility spellings
/// select the same Config child as `/config`, without rebuilding status or
/// usage state in the presentation layer.
pub struct SettingsShellSnapshotService {
    status: Arc<StatusState>,
    models: Arc<heycode_llm::CatalogRegistry>,
    sessions: Arc<heycode_session::SessionQueryService>,
}

impl SettingsShellSnapshotService {
    /// Request the canonical Config child with an explicitly unrefreshed
    /// doctor projection. Only `/status` performs live health checks.
    pub async fn request_config(&self, agent: &heycode_agent::Agent) {
        self.request(agent, SettingsShellTab::Config, false).await;
    }

    async fn request(
        &self,
        agent: &heycode_agent::Agent,
        tab: SettingsShellTab,
        refresh_doctor: bool,
    ) {
        let sessions = self.sessions.clone();
        let stats_task = tokio::task::spawn_blocking(move || sessions.stats());
        let status = if refresh_doctor {
            match self.status.doctor_report().await {
                Ok(doctor) => SettingsShellSection::Ready {
                    text: render_status(
                        agent,
                        Some(&doctor),
                        &self.status.sandbox_report(),
                        &self.status.prompt.instruction_sources(),
                    ),
                },
                Err(_) => SettingsShellSection::Failed {
                    message: "status\nstate: failed (effective health checks could not be read)"
                        .to_owned(),
                },
            }
        } else {
            SettingsShellSection::Ready {
                text: render_status(
                    agent,
                    None,
                    &self.status.sandbox_report(),
                    &self.status.prompt.instruction_sources(),
                ),
            }
        };
        let usage_text = render_usage(agent, &self.models);
        let usage = if usage_text == "usage\nstate: unavailable" {
            SettingsShellSection::Unavailable { reason: usage_text }
        } else {
            SettingsShellSection::Ready { text: usage_text }
        };
        let (stats, stats_snapshot) = match stats_task.await {
            Ok(result) => stats_section(result),
            Err(_) => (
                SettingsShellSection::Failed {
                    message: "stats\nstate: failed (durable session statistics could not be read)"
                        .to_owned(),
                },
                None,
            ),
        };
        let snapshot = SettingsShellSnapshot {
            status,
            usage,
            stats,
            stats_snapshot,
        };
        agent
            .ui()
            .emit(UiEvent::SettingsShellRequested { tab, snapshot });
    }
}

fn stats_section(
    result: Result<heycode_session::SessionStatsSnapshot, heycode_session::SessionQueryError>,
) -> (
    SettingsShellSection,
    Option<Box<heycode_session::SessionStatsSnapshot>>,
) {
    match result {
        Ok(snapshot) if snapshot.is_empty() => (
            SettingsShellSection::Empty {
                message: "stats\nstate: empty (no durable session activity yet)".to_owned(),
            },
            None,
        ),
        Ok(snapshot) => {
            let text = render_stats(&snapshot);
            (
                SettingsShellSection::Ready { text },
                Some(Box::new(snapshot)),
            )
        }
        Err(heycode_session::SessionQueryError::StatisticsUnavailable) => (
            SettingsShellSection::Unavailable {
                reason: "stats\nstate: unavailable (session provider has no statistics owner)"
                    .to_owned(),
            },
            None,
        ),
        Err(_) => (
            SettingsShellSection::Failed {
                message: "stats\nstate: failed (durable session statistics could not be read)"
                    .to_owned(),
            },
            None,
        ),
    }
}

fn render_stats(stats: &heycode_session::SessionStatsSnapshot) -> String {
    let partial = stats.unreadable_sessions() > 0;
    let lower_bound = if partial { " at least" } else { "" };
    let token_lower_bound = if partial || stats.unreported_assistant_messages() > 0 {
        " at least"
    } else {
        ""
    };
    let cache_write_lower_bound = if partial || stats.unknown_cache_write_reports() > 0 {
        " at least"
    } else {
        ""
    };
    let mut lines = vec![
        "stats".to_owned(),
        "scope: local durable sessions · active + archived · UTC".to_owned(),
        format!(
            "sessions{lower_bound}: used={} · active={} · archived={} · readable={} · unreadable={}",
            stats.used_sessions(),
            stats.active_used_sessions(),
            stats.archived_used_sessions(),
            stats.readable_sessions(),
            stats.unreadable_sessions(),
        ),
        format!(
            "messages{lower_bound}: total={} · user={} · assistant={}",
            stats.total_messages(),
            stats.user_messages(),
            stats.assistant_messages(),
        ),
        format!(
            "activity: active days={} of {} · current streak={} · longest streak={}",
            stats.active_days(),
            stats.calendar_days(),
            stats.current_streak_days(),
            stats.longest_streak_days(),
        ),
    ];
    if let (Some(first), Some(last)) = (stats.days().first(), stats.days().last()) {
        lines.push(format!(
            "dates: first={} · last={}",
            first.date(),
            last.date()
        ));
    } else {
        lines.push("dates: unavailable (no timestamped messages)".to_owned());
    }
    lines.push(stats.peak_hour_utc().map_or_else(
        || "peak hour: unavailable (no timestamped messages)".to_owned(),
        |hour| format!("peak hour: {hour:02}:00–{hour:02}:59 UTC"),
    ));
    lines.push(stats.longest_session_span_ms().map_or_else(
        || "longest recorded message span: unavailable".to_owned(),
        |duration| {
            format!(
                "longest recorded message span: {} · recorded sessions={}",
                render_duration(duration),
                stats.recorded_session_spans()
            )
        },
    ));
    lines.push(format!(
        "reported tokens{token_lower_bound}: input={} · output={} · total={}",
        stats.reported_input_tokens(),
        stats.reported_output_tokens(),
        stats
            .reported_input_tokens()
            .saturating_add(stats.reported_output_tokens()),
    ));
    lines.push(format!(
        "usage gaps: unreported responses={} · unattributed responses={}",
        stats.unreported_assistant_messages(),
        stats.unattributed_assistant_messages(),
    ));
    lines.push(format!(
        "cache: read={} · write{cache_write_lower_bound}={} · detailed reports={} · unknown writes={}",
        stats.cache_read_tokens(),
        stats.cache_write_tokens(),
        stats.cache_reports(),
        stats.unknown_cache_write_reports(),
    ));
    if partial {
        lines.push(format!(
            "coverage: partial ({} unreadable session log(s) excluded)",
            stats.unreadable_sessions()
        ));
    } else {
        lines.push("coverage: complete for the bounded local store scan".to_owned());
    }
    if let Some(favorite) = stats.models().first() {
        lines.push(format!(
            "favorite model: {}/{} · responses={} · reported={}",
            favorite.provider(),
            favorite.model(),
            favorite.responses(),
            favorite.reported_responses(),
        ));
    } else {
        lines.push("favorite model: unavailable (no attributable response)".to_owned());
    }
    for model in stats.models().iter().take(8) {
        lines.push(format!(
            "model {}/{}: responses={} · reported={} · input={} · output={}",
            model.provider(),
            model.model(),
            model.responses(),
            model.reported_responses(),
            model.reported_input_tokens(),
            model.reported_output_tokens(),
        ));
    }
    if stats.models().len() > 8 {
        lines.push(format!(
            "models: {} more route(s) omitted from this view",
            stats.models().len() - 8
        ));
    }
    if !stats.days().is_empty() {
        lines.push("recent activity:".to_owned());
        for day in stats.days().iter().rev().take(14).rev() {
            lines.push(format!(
                "  {} · sessions={} · messages={} · input={} · output={}",
                day.date(),
                day.sessions(),
                day.user_messages().saturating_add(day.assistant_messages()),
                day.reported_input_tokens(),
                day.reported_output_tokens(),
            ));
        }
        if stats.days().len() > 14 {
            lines.push(format!(
                "  … {} earlier active day(s) omitted",
                stats.days().len() - 14
            ));
        }
    }
    lines.join("\n")
}

fn render_duration(milliseconds: u64) -> String {
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

impl StatusState {
    async fn doctor_report(&self) -> anyhow::Result<DoctorReport> {
        Ok(self.doctor.run(self.lifecycle.child_token()).await?)
    }

    fn sandbox_report(&self) -> SandboxCapabilityReport {
        self.sandbox.capability_report()
    }
}

fn commands_for(
    state: Arc<StatusState>,
    shell: Arc<SettingsShellSnapshotService>,
    source: CommandSource,
) -> anyhow::Result<Vec<Arc<dyn Command>>> {
    Ok(vec![
        Arc::new(StatusCommand {
            descriptor: descriptor(
                "status",
                "Show effective runtime, route, permission, sandbox and health",
                source.clone(),
            )?,
            shell: shell.clone(),
        }),
        Arc::new(DoctorCommand {
            descriptor: descriptor(
                "doctor",
                "Run every live plugin-contributed health check",
                source.clone(),
            )?,
            state: state.clone(),
        }),
        Arc::new(PermissionCommand {
            descriptor: CommandDescriptor::new(
                "permissions",
                "Choose when we ask for permission",
                vec![heycode_agent::CommandArgument::optional(
                    "mode",
                    "full_access | accepted_edits | default | plan",
                )?],
                CommandTiming::Immediate,
                source.clone(),
            )?,
            state: state.clone(),
        }),
        Arc::new(SandboxCommand {
            descriptor: descriptor(
                "sandbox",
                "Inspect effective sandbox mode and host capabilities",
                source.clone(),
            )?,
            state: state.clone(),
        }),
        Arc::new(ConfigCommand {
            descriptor: CommandDescriptor::new(
                "config",
                "Open settings or show effective configuration provenance",
                vec![heycode_agent::CommandArgument::optional("action", "show")?],
                CommandTiming::Immediate,
                source,
            )?,
            state,
            shell,
        }),
    ])
}

struct ConfigCommand {
    descriptor: CommandDescriptor,
    state: Arc<StatusState>,
    shell: Arc<SettingsShellSnapshotService>,
}

#[async_trait]
impl Command for ConfigCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        match args.trim() {
            "" => {
                self.shell
                    .request(agent, SettingsShellTab::Config, false)
                    .await;
                Ok(())
            }
            "show" => {
                let text = self.state.config.as_ref().map_or_else(
                    || {
                        "config: this surface was composed without a configuration report"
                            .to_owned()
                    },
                    heycode_config::ConfigReport::render,
                );
                agent.ui().emit(UiEvent::Info { text });
                Ok(())
            }
            _ => anyhow::bail!("usage: /config [show]"),
        }
    }
}

fn context_commands(
    models: Arc<heycode_llm::CatalogRegistry>,
    shell: Arc<SettingsShellSnapshotService>,
    source: CommandSource,
) -> anyhow::Result<Vec<Arc<dyn Command>>> {
    Ok(vec![
        Arc::new(ContextCommand {
            descriptor: descriptor(
                "context",
                "Explain the latest request context, evidence, cost and strategies",
                source.clone(),
            )?,
            models: models.clone(),
        }),
        Arc::new(UsageCommand {
            descriptor: descriptor(
                "usage",
                "Show durable session usage, route, completeness and cost",
                source.clone(),
            )?,
            shell: shell.clone(),
        }),
        Arc::new(StatsCommand {
            descriptor: descriptor(
                "stats",
                "Show cross-session activity statistics when available",
                source,
            )?,
            shell,
        }),
    ])
}

struct ContextCommand {
    descriptor: CommandDescriptor,
    models: Arc<heycode_llm::CatalogRegistry>,
}

#[async_trait]
impl Command for ContextCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "context")?;
        agent.ui().emit(UiEvent::Info {
            text: render_context(agent, &self.models),
        });
        Ok(())
    }
}

struct UsageCommand {
    descriptor: CommandDescriptor,
    shell: Arc<SettingsShellSnapshotService>,
}

#[async_trait]
impl Command for UsageCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "usage")?;
        self.shell
            .request(agent, SettingsShellTab::Usage, false)
            .await;
        Ok(())
    }
}

struct StatsCommand {
    descriptor: CommandDescriptor,
    shell: Arc<SettingsShellSnapshotService>,
}

#[async_trait]
impl Command for StatsCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "stats")?;
        self.shell
            .request(agent, SettingsShellTab::Stats, false)
            .await;
        Ok(())
    }
}

/// Cells in the `/context` occupancy grid: ten rows of twenty.
const CONTEXT_GRID_CELLS: u64 = 200;

/// One occupied cell.
const CONTEXT_CELL_USED: &str = "⛀";
/// One free cell, and the free-space swatch in the legend.
const CONTEXT_CELL_FREE: &str = "⛶";
/// The category swatch in the legend.
const CONTEXT_CELL_SWATCH: &str = "⛁";

/// Human label for one measured envelope contributor.
const fn contributor_label(contributor: heycode_llm::EnvelopeContributor) -> &'static str {
    match contributor {
        heycode_llm::EnvelopeContributor::System => "System instructions",
        heycode_llm::EnvelopeContributor::Guidance => "Guidance and skills",
        heycode_llm::EnvelopeContributor::Messages => "Conversation",
        heycode_llm::EnvelopeContributor::ToolResults => "Tool results",
        heycode_llm::EnvelopeContributor::Tools => "Tool schemas",
        heycode_llm::EnvelopeContributor::ProviderState => "Opaque provider state",
        heycode_llm::EnvelopeContributor::Attachments => "Attachments",
    }
}

/// Same labels, resolved from the durable contributor name persisted with a
/// request, so a restored measurement reads identically to a live one.
fn restored_contributor_label(name: &str) -> String {
    match name {
        "system" => "System instructions".to_owned(),
        "guidance" => "Guidance and skills".to_owned(),
        "messages" => "Conversation".to_owned(),
        "tool_results" => "Tool results".to_owned(),
        "tools" => "Tool schemas".to_owned(),
        "provider_state" => "Opaque provider state".to_owned(),
        "attachments" => "Attachments".to_owned(),
        other => other.to_owned(),
    }
}

/// `1.3k`, `998.7k`, `1m` — the reference's compact token scale.
fn compact_tokens(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value < 1_000_000 {
        return format!("{:.1}k", value as f64 / 1_000_f64);
    }
    format!("{:.1}m", value as f64 / 1_000_000_f64)
}

/// One measured or estimated category with a token count.
struct ContextCategory {
    label: String,
    tokens: u64,
}

/// The occupancy grid and its right-hand legend.
///
/// Cells are allocated proportionally against the window, with one cell
/// reserved for any category that has tokens at all, so a small but real
/// contributor is visible rather than rounded away. When nothing has been
/// measured the grid is entirely free space and the legend says so instead of
/// implying a measurement of zero.
fn context_grid_lines(
    window: Option<u64>,
    used: Option<u64>,
    categories: &[ContextCategory],
    head: &[String],
    caption: &str,
) -> Vec<String> {
    let mut cells: Vec<(usize, u64)> = Vec::new();
    if let Some(window) = window.filter(|window| *window > 0) {
        let mut remaining = CONTEXT_GRID_CELLS;
        for (index, category) in categories.iter().enumerate() {
            if category.tokens == 0 || remaining == 0 {
                continue;
            }
            let share = (u128::from(category.tokens) * u128::from(CONTEXT_GRID_CELLS)
                / u128::from(window)) as u64;
            let count = share.max(1).min(remaining);
            remaining -= count;
            cells.push((index, count));
        }
    }
    let mut glyphs = Vec::with_capacity(CONTEXT_GRID_CELLS as usize);
    for (_, count) in &cells {
        glyphs.extend(std::iter::repeat_n(
            CONTEXT_CELL_USED,
            usize::try_from(*count).unwrap_or(0),
        ));
    }
    while glyphs.len() < CONTEXT_GRID_CELLS as usize {
        glyphs.push(CONTEXT_CELL_FREE);
    }
    let mut legend: Vec<String> = head.to_vec();
    legend.push(String::new());
    legend.push(caption.to_owned());
    let percent = |tokens: u64| {
        window
            .filter(|window| *window > 0)
            .map_or_else(String::new, |window| {
                format!(" ({:.1}%)", tokens as f64 * 100.0 / window as f64)
            })
    };
    for category in categories.iter().filter(|category| category.tokens > 0) {
        legend.push(format!(
            "{CONTEXT_CELL_SWATCH} {}: {} tokens{}",
            category.label,
            compact_tokens(category.tokens),
            percent(category.tokens)
        ));
    }
    match (window, used) {
        (Some(window), Some(used)) if window > 0 => {
            let free = window.saturating_sub(used);
            legend.push(format!(
                "{CONTEXT_CELL_FREE} Free space: {}{}",
                compact_tokens(free),
                percent(free)
            ));
        }
        (Some(_), None) => legend.push(format!(
            "{CONTEXT_CELL_FREE} Free space: the whole window — nothing measured yet"
        )),
        _ => legend.push(format!(
            "{CONTEXT_CELL_FREE} Free space: unknown (context window not stated)"
        )),
    }
    (0..10)
        .map(|row| {
            let grid = (0..20)
                .map(|column| glyphs[row * 20 + column])
                .collect::<Vec<_>>()
                .join(" ");
            match legend.get(row).filter(|text| !text.is_empty()) {
                Some(text) => format!("{grid}   {text}"),
                None => grid,
            }
        })
        .collect()
}

fn render_context(agent: &heycode_agent::Agent, models: &heycode_llm::CatalogRegistry) -> String {
    let strategies = agent
        .compaction_strategies()
        .into_iter()
        .map(|strategy| format!("{} ({})", strategy.id().as_str(), strategy.kind().name()))
        .collect::<Vec<_>>()
        .join(", ");
    let envelope = agent.token_envelope();
    let (route, durable_window) = latest_request_facts(agent);
    // A provider that has not stated its window yet (or a fake) still leaves
    // the configured ceiling, which is what compaction actually uses; a
    // percentage against it beats "window unknown".
    let current_budget = agent
        .context_budget()
        .or_else(|| restored_context_budget(agent));
    let window = current_budget.as_ref().map_or_else(
        || durable_window.or_else(|| Some(agent.context_window()).filter(|w| *w > 0)),
        |budget| budget.window,
    );
    let window_note = if durable_window.is_none() && window.is_some() {
        " (configured window)"
    } else {
        ""
    };
    let projection = envelope
        .as_ref()
        .map(|envelope| heycode_llm::ContextProjection::explain(envelope, window));
    // The occupancy grid leads, the way the reference does; the evidence lines
    // that follow are heycode's own and are not dropped for the sake of the shape.
    let mut categories: Vec<ContextCategory> = Vec::new();
    let mut estimated = false;
    if let Some(projection) = &projection {
        for line in projection.lines() {
            let tokens = match &line.tokens {
                heycode_llm::ContributorTokens::Exact(tokens) => *tokens,
                heycode_llm::ContributorTokens::Estimated(tokens, _) => {
                    estimated = true;
                    *tokens
                }
                heycode_llm::ContributorTokens::Uncounted(_) => continue,
            };
            categories.push(ContextCategory {
                label: contributor_label(line.contributor).to_owned(),
                tokens,
            });
        }
    } else if let Some(restored) = restored_contributor_measurements(agent) {
        estimated = restored.1;
        categories = restored.0;
    }
    let head = match &route {
        Some((provider, model)) => vec![model.clone(), format!("{provider}/{model}")],
        None => vec![
            "route not yet recorded".to_owned(),
            "no durable request header".to_owned(),
        ],
    };
    let used = current_budget
        .as_ref()
        .map(|budget| budget.used)
        .or_else(|| {
            (!categories.is_empty()).then(|| categories.iter().map(|row| row.tokens).sum())
        });
    let head = {
        let mut head = head;
        head.push(match (used, window) {
            (Some(used), Some(window)) if window > 0 => format!(
                "{}/{} tokens ({:.0}%)",
                compact_tokens(used),
                compact_tokens(window),
                used as f64 * 100.0 / window as f64
            ),
            (Some(used), _) => format!("{} tokens · window unknown", compact_tokens(used)),
            _ => "no measurement yet".to_owned(),
        });
        head
    };
    let caption = if categories.is_empty() {
        "Nothing measured yet — no request envelope has been committed"
    } else if estimated {
        "Estimated usage by category"
    } else {
        "Measured usage by category"
    };
    let mut lines = vec!["Context Usage".to_owned()];
    lines.extend(context_grid_lines(
        window,
        used,
        &categories,
        &head,
        caption,
    ));
    lines.push(String::new());
    lines.push("context".to_owned());
    if let Ok(session) = agent.session().lock() {
        lines.push(format!("session: {} (current agent only)", session.id()));
        if let Some((request_id, turn, step, time_ms)) =
            session
                .events()
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    heycode_session::SessionEventKind::RequestHeader {
                        request_id,
                        turn,
                        step,
                        ..
                    } => Some((request_id, turn, step, event.time_ms)),
                    _ => None,
                })
        {
            lines.push(format!("request: {request_id} · turn {turn}, step {step}"));
            lines.push(format!("request recorded: {time_ms} ms since Unix epoch"));
        }
    }
    if let Some(budget) = &current_budget {
        lines.push(format!(
            "capacity source: {:?}; measurement: {:?}",
            budget.limit_source, budget.confidence
        ));
        lines.push(format!(
            "current context: {} tokens{}; contributors below describe the latest admitted request",
            budget.used,
            if budget.projected {
                " (projected after response/tool output)"
            } else {
                ""
            }
        ));
        lines.push(format!(
            "reserved output: {}; safety margin: {}",
            budget.output_reserve, budget.safety_margin
        ));
        lines.push(format!(
            "auto compaction: {}; state: {:?}; trigger: {}; tokens until trigger: {}",
            if budget.auto_compact { "on" } else { "off" },
            budget.activity,
            budget
                .compact_at
                .map_or_else(|| "unknown".to_owned(), |v| v.to_string()),
            budget.compact_at.map_or_else(
                || "unknown".to_owned(),
                |v| v.saturating_sub(budget.used).to_string()
            )
        ));
        lines.push(format!(
            "usable input remaining{}: {}",
            if budget.confidence == heycode_llm::ContextConfidence::AtLeast {
                " (at most)"
            } else {
                ""
            },
            budget
                .remaining()
                .map_or_else(|| "unknown".to_owned(), |v| v.to_string())
        ));
        if let Some(before) = budget.before_compaction {
            lines.push(format!(
                "last compaction: {before} -> {} input tokens",
                budget.after_compaction.unwrap_or(budget.used)
            ));
        }
    }
    lines.push(route.as_ref().map_or_else(
        || "route: unknown (no durable request header)".to_owned(),
        |(provider, model)| format!("route: {provider}/{model}"),
    ));
    if let Some(projection) = &projection {
        for line in projection.lines() {
            let evidence = match &line.tokens {
                heycode_llm::ContributorTokens::Exact(tokens) => format!("exact {tokens}"),
                heycode_llm::ContributorTokens::Estimated(tokens, method) => {
                    format!("estimated {tokens} ({})", method.name())
                }
                heycode_llm::ContributorTokens::Uncounted(reason) => {
                    format!("uncounted ({})", reason.name())
                }
            };
            let refusals = if line.refusals.is_empty() {
                String::new()
            } else {
                format!(
                    " · refused={}",
                    line.refusals
                        .iter()
                        .map(|refusal| format!("{}:{}", refusal.counter(), refusal.message()))
                        .collect::<Vec<_>>()
                        .join("|")
                )
            };
            lines.push(format!("{}: {evidence}{refusals}", line.contributor.name()));
        }
        lines.push(render_context_total(projection.total()));
        lines.push(format!(
            "{}{window_note}",
            render_budget(projection.budget())
        ));
        if projection
            .lines()
            .iter()
            .any(|line| line.contributor == heycode_llm::EnvelopeContributor::Guidance)
        {
            lines.push(
                "System/guidance estimates allocate the system total by assembled prompt bytes."
                    .to_owned(),
            );
        }
    } else if let Some(restored) = restored_contributor_lines(agent) {
        lines.extend(restored);
    } else {
        lines.push("contributors: unavailable (no retained contributor measurements)".to_owned());
        if current_budget.is_none() {
            lines.push("measurement: unavailable (no committed request envelope)".to_owned());
        }
    }
    lines.extend(configuration_lines(agent));
    lines.push(retry_line(agent));
    let pricing = route
        .as_ref()
        .and_then(|(provider, model)| cached_pricing(models, provider, model));
    if let Some(envelope) = &envelope {
        lines.push(render_envelope_cost(
            envelope,
            pricing
                .as_ref()
                .unwrap_or(&heycode_llm::ModelPricing::unknown()),
        ));
    }
    match latest_response_metadata(agent) {
        Ok(Some(metadata)) => lines.extend(render_response_metadata("detailed", &metadata)),
        Ok(None) => {
            lines.push("detailed cache: unavailable (no durable provider cache facts)".to_owned())
        }
        Err(()) => lines.push(
            "detailed cache: unavailable (durable provider response metadata is invalid)"
                .to_owned(),
        ),
    }
    lines.push(format!("compaction strategies: {strategies}"));
    lines.join("\n")
}

/// The latest request's persisted per-contributor counts, for the grid.
///
/// Returns the categories and whether any of them was an estimate rather than
/// an exact count; uncounted contributors are omitted rather than shown as
/// zero, because "not counted" and "no tokens" are different facts.
fn restored_contributor_measurements(
    agent: &heycode_agent::Agent,
) -> Option<(Vec<ContextCategory>, bool)> {
    use heycode_session::RequestContributorMeasurement as Measurement;
    let session = agent.session().lock().ok()?;
    let request = heycode_session::project_requests(session.events())
        .ok()?
        .pop()?;
    let contributors = request.context.contributors?;
    let mut estimated = false;
    let mut categories = Vec::new();
    for entry in contributors {
        let tokens = match entry.measurement {
            Measurement::Exact { tokens } => tokens,
            Measurement::Estimated { tokens, .. } => {
                estimated = true;
                tokens
            }
            Measurement::Uncounted { .. } => continue,
        };
        categories.push(ContextCategory {
            label: restored_contributor_label(&entry.contributor),
            tokens,
        });
    }
    Some((categories, estimated))
}

fn restored_contributor_lines(agent: &heycode_agent::Agent) -> Option<Vec<String>> {
    use heycode_session::RequestContributorMeasurement as Measurement;
    let session = agent.session().lock().ok()?;
    let request = heycode_session::project_requests(session.events())
        .ok()?
        .pop()?;
    let contributors = request.context.contributors?;
    let mut lines = vec!["contributors: restored from the latest request".to_owned()];
    let mut counted = 0u64;
    let mut estimated = false;
    let mut uncounted = Vec::new();
    let attributed_guidance = contributors
        .iter()
        .any(|entry| entry.contributor == "guidance");
    for entry in contributors {
        let evidence = match entry.measurement {
            Measurement::Exact { tokens } => {
                counted = counted.saturating_add(tokens);
                format!("exact {tokens}")
            }
            Measurement::Estimated { tokens, method } => {
                estimated = true;
                counted = counted.saturating_add(tokens);
                format!("estimated {tokens} ({method})")
            }
            Measurement::Uncounted { reason } => {
                uncounted.push(format!("{}:{reason}", entry.contributor));
                format!("uncounted ({reason})")
            }
        };
        let refusals = if entry.refusals.is_empty() {
            String::new()
        } else {
            format!(" · refused={}", entry.refusals.join("|"))
        };
        lines.push(format!("{}: {evidence}{refusals}", entry.contributor));
    }
    if attributed_guidance {
        lines.push(
            "System/guidance estimates allocate the system total by assembled prompt bytes."
                .to_owned(),
        );
    }
    lines.push(if !uncounted.is_empty() {
        format!(
            "total: at least {counted} · uncounted={}",
            uncounted.join(",")
        )
    } else {
        format!(
            "total: {} {counted}",
            if estimated { "estimated" } else { "exact" }
        )
    });
    Some(lines)
}

// Restore only the latest request's persisted measurement; never re-tokenize
// earlier content or borrow an older request's contributor allocations.
fn restored_context_budget(agent: &heycode_agent::Agent) -> Option<heycode_llm::ContextBudget> {
    let session = agent.session().lock().ok()?;
    let request = heycode_session::project_requests(session.events())
        .ok()?
        .pop()?;
    let mut budget = *request.context.budget?;
    let boundary = session.events().iter().rposition(|event| {
        matches!(
            &event.kind, heycode_session::SessionEventKind::RequestContext { request_id, .. }
                if request_id == &request.request_id
        )
    })?;
    for event in &session.events()[boundary + 1..] {
        if let Some(growth) = heycode_session::retained_context_growth(&event.kind) {
            budget.used = budget.used.saturating_add(growth.tokens);
            budget.projected = true;
            if growth.uncounted {
                budget.confidence = heycode_llm::ContextConfidence::AtLeast;
            } else if budget.confidence == heycode_llm::ContextConfidence::Exact {
                budget.confidence = heycode_llm::ContextConfidence::Estimated;
            }
        }
    }
    Some(budget)
}

fn configuration_lines(agent: &heycode_agent::Agent) -> Vec<String> {
    let Ok(session) = agent.session().lock() else {
        return vec!["configuration: unavailable".into()];
    };
    let Some(configuration) = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            heycode_session::SessionEventKind::RequestHeader { header, .. } => {
                Some(header.configuration.as_ref())
            }
            _ => None,
        })
        .flatten()
    else {
        return vec!["configuration: not recorded for the latest request".into()];
    };
    let changed = if configuration.changed.is_empty() {
        "unchanged".into()
    } else {
        configuration
            .changed
            .iter()
            .map(|change| match change {
                heycode_session::RequestConfigurationChange::Initial => "initial",
                heycode_session::RequestConfigurationChange::System => "system/guidance",
                heycode_session::RequestConfigurationChange::Tools => "tool definitions",
                heycode_session::RequestConfigurationChange::Route => "route/binding",
                heycode_session::RequestConfigurationChange::Options => "request options",
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    vec![
        format!(
            "configuration revision {}: {changed}",
            configuration.revision
        ),
        format!("configuration SHA-256: {}", configuration.sha256),
        format!("system SHA-256: {}", configuration.system_sha256),
        format!("tools SHA-256: {}", configuration.tools_sha256),
        "Configuration fingerprints exclude conversation content and do not establish a cache hit."
            .into(),
    ]
}

fn latest_request_facts(agent: &heycode_agent::Agent) -> (Option<(String, String)>, Option<u64>) {
    let Ok(session) = agent.session().lock() else {
        return (None, None);
    };
    let header = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            heycode_session::SessionEventKind::RequestHeader {
                request_id, header, ..
            } => Some((
                request_id.clone(),
                header.provider.clone(),
                header.model.clone(),
            )),
            _ => None,
        });
    let Some((request_id, provider, model)) = header else {
        return (None, None);
    };
    let window = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            heycode_session::SessionEventKind::RequestContext {
                request_id: candidate,
                context,
            } if candidate == &request_id => Some(context.context_window),
            _ => None,
        });
    (Some((provider, model)), window.flatten())
}

fn latest_response_metadata(
    agent: &heycode_agent::Agent,
) -> Result<Option<heycode_core::ProviderResponseMetadata>, ()> {
    let session = agent.session().lock().map_err(|_| ())?;
    Ok(heycode_session::project_requests(session.events())
        .map_err(|_| ())?
        .into_iter()
        .next_back()
        .and_then(|request| request.response_metadata))
}

fn cached_pricing(
    models: &heycode_llm::CatalogRegistry,
    provider: &str,
    model: &str,
) -> Option<heycode_llm::ModelPricing> {
    models
        .cached(provider)
        .ok()?
        .models
        .iter()
        .find(|descriptor| {
            descriptor.id == model || descriptor.aliases.iter().any(|id| id == model)
        })
        .map(|descriptor| descriptor.pricing.clone())
}

fn render_context_total(total: &heycode_llm::EnvelopeTotal) -> String {
    match total {
        heycode_llm::EnvelopeTotal::Exact(tokens) => format!("total: exact {tokens}"),
        heycode_llm::EnvelopeTotal::Estimated(tokens) => format!("total: estimated {tokens}"),
        heycode_llm::EnvelopeTotal::AtLeast { counted, uncounted } => format!(
            "total: at least {counted} · uncounted={}",
            uncounted
                .iter()
                .map(|(contributor, reason)| format!("{}:{}", contributor.name(), reason.name()))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// The replay policy the newest durable request ran under.
///
/// A duplicate dispatch is either policy or a bug, and only the recorded
/// policy can tell them apart; a record written before the field existed says
/// "not recorded" rather than inventing a safe-sounding default.
fn retry_line(agent: &heycode_agent::Agent) -> String {
    let Ok(session) = agent.session().lock() else {
        return "retry: unavailable (session is locked)".to_owned();
    };
    let recorded = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            heycode_session::SessionEventKind::RequestHeader { header, .. } => {
                Some(header.options.retry)
            }
            _ => None,
        });
    match recorded {
        Some(Some(retry)) => format!(
            "retry: {} attempt(s) · {}",
            retry.max_attempts,
            match retry.safety {
                heycode_session::RequestRetrySafetySnapshot::Never => "never replayed",
                heycode_session::RequestRetrySafetySnapshot::DefinitiveFailuresOnly =>
                    "definitive failures only",
                heycode_session::RequestRetrySafetySnapshot::StatelessPreOutput =>
                    "replayable until output starts",
            }
        ),
        Some(None) => "retry: not recorded by the request that wrote this header".to_owned(),
        None => "retry: unavailable (no durable request header)".to_owned(),
    }
}

fn render_budget(budget: &heycode_llm::BudgetUse) -> String {
    match budget {
        heycode_llm::BudgetUse::Measured {
            used,
            window,
            remaining,
        } => format!("budget: used {used}/{window} · remaining {remaining}"),
        heycode_llm::BudgetUse::AtLeast {
            used_at_least,
            window,
            remaining_at_most,
        } => format!(
            "budget: used at least {used_at_least}/{window} · remaining at most {remaining_at_most}"
        ),
        heycode_llm::BudgetUse::WindowUnknown { total } => {
            format!("budget: window unknown · {}", render_context_total(total))
        }
        _ => "budget: unavailable (unrecognized evidence class)".to_owned(),
    }
}

fn render_envelope_cost(
    envelope: &heycode_llm::TokenEnvelope,
    pricing: &heycode_llm::ModelPricing,
) -> String {
    match envelope.cost(pricing) {
        heycode_llm::EnvelopeCost::Exact {
            pico_units,
            currency,
        } => format!("input cost: exact {pico_units} pico-{}", currency.code()),
        heycode_llm::EnvelopeCost::AtLeast {
            pico_units,
            currency,
        } => format!("input cost: at least {pico_units} pico-{}", currency.code()),
        heycode_llm::EnvelopeCost::Unpriced => {
            "input cost: unknown (input price unavailable)".to_owned()
        }
    }
}

/// Sum the lines a recorded `edit`/`write` diff added and removed.
///
/// Only diffs the session actually stored are counted; nothing is re-read from
/// the workspace, so the answer describes this conversation and not the tree.
fn recorded_code_changes(events: &[heycode_session::SessionEvent]) -> (u64, u64) {
    let mut editing = std::collections::HashSet::new();
    let (mut added, mut removed) = (0_u64, 0_u64);
    for event in events {
        match &event.kind {
            heycode_session::SessionEventKind::ToolCall { call_id, name, .. }
                if matches!(name.as_str(), "edit" | "write") =>
            {
                editing.insert(call_id.as_str().to_owned());
            }
            heycode_session::SessionEventKind::ToolResult {
                call_id,
                content,
                is_error: false,
                ..
            } if editing.contains(call_id.as_str()) => {
                let Some(diff) = serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("diff")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                else {
                    continue;
                };
                for line in diff.lines() {
                    if line.starts_with("+++") || line.starts_with("---") {
                        continue;
                    }
                    if line.starts_with('+') {
                        added = added.saturating_add(1);
                    } else if line.starts_with('-') {
                        removed = removed.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
    }
    (added, removed)
}

/// Render milliseconds the way the reference does: `0s`, `42s`, `3m 7s`.
fn compact_duration_ms(total: u64) -> String {
    let seconds = total / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// The aligned `Session` block the reference opens `/cost` with.
///
/// Every row is either a measurement this session actually holds or the word
/// `unknown` with the reason. An unpriced route reports `unknown`, never
/// `$0.00`, because a zero cost and an unknown cost are different facts.
fn session_block(
    events: &[heycode_session::SessionEvent],
    usage: &heycode_session::SessionUsage,
    models: &heycode_llm::CatalogRegistry,
    detailed: Option<&Vec<(String, String, heycode_core::ProviderResponseMetadata)>>,
    reported_steps: usize,
) -> Vec<String> {
    let at_least = if usage.is_complete() { "" } else { " at least" };
    let cost = match session_cost(usage, models, detailed, reported_steps) {
        Ok((pico_units, currency)) => format!(
            "{at_least} {:.4} {}",
            pico_units as f64 / 1e12_f64,
            currency.code()
        )
        .trim_start()
        .to_owned(),
        Err(reason) => format!("unknown ({reason})"),
    };
    let wall = match (events.first(), events.last()) {
        (Some(first), Some(last)) => compact_duration_ms(
            u64::try_from(last.time_ms.saturating_sub(first.time_ms)).unwrap_or(0),
        ),
        _ => "0s".to_owned(),
    };
    let turn_spans = usage
        .turns()
        .iter()
        .filter_map(|turn| turn.duration_ms)
        .sum::<u64>();
    let api = if usage.turns().iter().any(|turn| turn.duration_ms.is_some()) {
        format!(
            "{}{at_least} (measured turn spans)",
            compact_duration_ms(turn_spans)
        )
    } else {
        "unknown (no settled turn was measured)".to_owned()
    };
    let (added, removed) = recorded_code_changes(events);
    let reported = usage.reported_tokens();
    let (cache_read, cache_write) = detailed.map_or((0, 0), |rows| {
        rows.iter()
            .filter_map(|(_, _, metadata)| metadata.cache_usage())
            .fold((0_u64, 0_u64), |(read, write), cache| {
                (
                    read.saturating_add(cache.cache_read_tokens()),
                    write.saturating_add(cache.reported_cache_write_tokens().unwrap_or(0)),
                )
            })
    });
    vec![
        "Session".to_owned(),
        String::new(),
        format!("Total cost:            {cost}"),
        format!("Total duration (API):  {api}"),
        format!("Total duration (wall): {wall}"),
        format!("Total code changes:    {added} lines added, {removed} lines removed"),
        format!(
            "Usage:                 {} input, {} output, {cache_read} cache read, {cache_write} cache write{at_least}",
            reported.prompt_tokens, reported.completion_tokens
        ),
        String::new(),
    ]
}

fn render_usage(agent: &heycode_agent::Agent, models: &heycode_llm::CatalogRegistry) -> String {
    let (events, usage, detailed, reported_steps) = {
        let Ok(session) = agent.session().lock() else {
            return "usage\nstate: unavailable".to_owned();
        };
        let usage = heycode_session::project_usage(session.events());
        let detailed = heycode_session::project_requests(session.events()).map(|requests| {
            requests
                .into_iter()
                .filter_map(|request| {
                    request
                        .response_metadata
                        .map(|metadata| (request.header.provider, request.header.model, metadata))
                })
                .collect::<Vec<_>>()
        });
        let reported_steps = session
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    heycode_session::SessionEventKind::AssistantMessage { usage: Some(_), .. }
                )
            })
            .count();
        (session.events().to_vec(), usage, detailed, reported_steps)
    };
    let reported = usage.reported_tokens();
    let mut lines = session_block(
        &events,
        &usage,
        models,
        detailed.as_ref().ok(),
        reported_steps,
    );
    lines.extend([
        "usage".to_owned(),
        format!("turns: {}", usage.turns().len()),
        format!(
            "reported tokens{}: input={} · output={} · total={}",
            if usage.is_complete() {
                ""
            } else {
                " (at least)"
            },
            reported.prompt_tokens,
            reported.completion_tokens,
            reported
                .prompt_tokens
                .saturating_add(reported.completion_tokens)
        ),
        format!("unreported steps: {}", usage.unreported_steps()),
        format!(
            "routes: {}",
            if usage.routes().is_empty() {
                "unknown".to_owned()
            } else {
                usage
                    .routes()
                    .into_iter()
                    .map(|(provider, model)| format!("{provider}/{model}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        render_session_cost(&usage, models, detailed.as_ref().ok(), reported_steps),
    ]);
    match detailed {
        Err(_) => lines.push(
            "detailed cache: unavailable (durable provider response metadata is invalid)"
                .to_owned(),
        ),
        Ok(detailed) if detailed.is_empty() => {
            lines.push("detailed cache: unavailable (no durable provider cache facts)".to_owned());
        }
        Ok(detailed) => {
            let omitted = detailed.len().saturating_sub(20);
            if omitted > 0 {
                lines.push(format!(
                    "detailed provider responses: {omitted} earlier response(s) omitted"
                ));
            }
            for (provider, model, metadata) in detailed.iter().rev().take(20).rev() {
                lines.extend(render_response_metadata(
                    &format!("detailed {provider}/{model}"),
                    metadata,
                ));
            }
        }
    }
    lines.extend(configuration_lines(agent));
    if usage.tools().is_empty() {
        lines.push("tool usage: none".to_owned());
    } else {
        for tool in usage.tools() {
            let cost = match &tool.cost {
                heycode_core::ServerToolUsageCost::Unknown => "unknown".to_owned(),
                heycode_core::ServerToolUsageCost::Published(cost) => {
                    format!("{} pico-{}", cost.pico_units(), cost.currency())
                }
            };
            lines.push(format!(
                "tool {}/{}: requests={} · success={} · error={} · unsettled={} · cost={cost}",
                tool.source.as_str(),
                tool.logical,
                tool.requests,
                tool.successes,
                tool.errors,
                tool.unsettled,
            ));
        }
    }
    let omitted = usage.turns().len().saturating_sub(20);
    if omitted > 0 {
        lines.push(format!("turn detail: {omitted} earlier turn(s) omitted"));
    }
    for turn in usage.turns().iter().rev().take(20).rev() {
        let tokens = turn.usage.map_or_else(
            || "unknown".to_owned(),
            |tokens| format!("{}/{}", tokens.prompt_tokens, tokens.completion_tokens),
        );
        let route = turn
            .route
            .as_ref()
            .map_or("unknown".to_owned(), |(provider, model)| {
                format!("{provider}/{model}")
            });
        let outcome = match turn.outcome {
            heycode_session::TurnOutcome::Settled(reason) => format!("{reason:?}"),
            heycode_session::TurnOutcome::Open => "open".to_owned(),
        };
        lines.push(format!(
            "turn {}: tokens={tokens} · route={route} · outcome={outcome}",
            turn.turn
        ));
    }
    lines.join("\n")
}

fn render_response_metadata(
    prefix: &str,
    metadata: &heycode_core::ProviderResponseMetadata,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(cache) = metadata.cache_usage() {
        let mut line = format!(
            "{prefix} cache: input={} · output={} · cache read={} · cache write={}",
            cache.input_tokens(),
            cache.output_tokens(),
            cache.cache_read_tokens(),
            cache
                .reported_cache_write_tokens()
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
        );
        if let Some(uncached) = cache.uncached_input_tokens() {
            line.push_str(&format!(" · uncached={uncached}"));
        }
        if let (Some(five), Some(hour)) =
            (cache.cache_write_5m_tokens(), cache.cache_write_1h_tokens())
        {
            line.push_str(&format!(" · write5m={five} · write1h={hour}"));
        }
        if let Some(reasoning) = cache.reasoning_tokens() {
            line.push_str(&format!(" · reasoning={reasoning}"));
        }
        lines.push(line);
    }
    if !metadata.context_edits().is_empty() || metadata.cache_prefix_impact().is_some() {
        let edits = metadata
            .context_edits()
            .iter()
            .map(|edit| {
                format!(
                    "{}={}/{} tokens",
                    match edit.kind() {
                        heycode_core::ContextEditKind::ClearThinking => "clear_thinking",
                        heycode_core::ContextEditKind::ClearToolUses => "clear_tool_uses",
                    },
                    edit.cleared_units(),
                    edit.cleared_input_tokens()
                )
            })
            .collect::<Vec<_>>();
        let impact = match metadata.cache_prefix_impact() {
            Some(heycode_core::CachePrefixImpact::Preserved) => "preserved",
            Some(heycode_core::CachePrefixImpact::InvalidatedAtEdit) => "invalidated_at_edit",
            None => "unknown",
        };
        lines.push(format!(
            "{prefix} context edits: {} · cache prefix={impact}",
            if edits.is_empty() {
                "none".to_owned()
            } else {
                edits.join(" · ")
            }
        ));
    }
    lines
}

/// Sum the session's derived cost, or say why it cannot be summed.
///
/// The cache-aware path is preferred whenever every reported step carries
/// detailed cache accounting; otherwise the plain per-turn path is used. An
/// `Err` carries the reason, which callers surface instead of a zero.
fn session_cost(
    usage: &heycode_session::SessionUsage,
    models: &heycode_llm::CatalogRegistry,
    detailed: Option<&Vec<(String, String, heycode_core::ProviderResponseMetadata)>>,
    reported_steps: usize,
) -> Result<(u128, heycode_llm::PriceCurrency), String> {
    if let Some(detailed) = detailed
        && detailed
            .iter()
            .any(|(_, _, metadata)| metadata.cache_usage().is_some())
    {
        if detailed.len() != reported_steps
            || detailed
                .iter()
                .any(|(_, _, metadata)| metadata.cache_usage().is_none())
        {
            return Err("detailed cache accounting incomplete".to_owned());
        }
        let mut total = 0_u128;
        let mut currency = None;
        for (provider, model, metadata) in detailed {
            let Some(pricing) = cached_pricing(models, provider, model) else {
                return Err("pricing unavailable".to_owned());
            };
            let Some(cache) = metadata.cache_usage() else {
                return Err("detailed cache accounting incomplete".to_owned());
            };
            let heycode_llm::RequestCost::Derived {
                pico_units,
                currency: row_currency,
            } = heycode_llm::RequestCost::derive_detailed(cache, &pricing)
            else {
                return Err("cache pricing incomplete".to_owned());
            };
            if currency.is_some_and(|current| current != row_currency) {
                return Err("mixed currencies".to_owned());
            }
            let Some(next) = total.checked_add(pico_units) else {
                return Err("cost overflow".to_owned());
            };
            total = next;
            currency = Some(row_currency);
        }
        return currency
            .map(|currency| (total, currency))
            .ok_or_else(|| "no priced usage".to_owned());
    }
    let mut pico_units = 0_u128;
    let mut currency = None;
    for turn in usage.turns() {
        let (Some(tokens), Some((provider, model))) = (turn.usage, turn.route.as_ref()) else {
            return Err("usage or route incomplete".to_owned());
        };
        let Some(pricing) = cached_pricing(models, provider, model) else {
            return Err("pricing unavailable".to_owned());
        };
        let heycode_llm::RequestCost::Derived {
            pico_units: row,
            currency: row_currency,
        } = heycode_llm::RequestCost::derive(tokens, &pricing)
        else {
            return Err("pricing incomplete".to_owned());
        };
        if currency.is_some_and(|current| current != row_currency) {
            return Err("mixed currencies".to_owned());
        }
        currency = Some(row_currency);
        pico_units = pico_units.saturating_add(row);
    }
    currency
        .map(|currency| (pico_units, currency))
        .ok_or_else(|| "no priced usage".to_owned())
}

fn render_session_cost(
    usage: &heycode_session::SessionUsage,
    models: &heycode_llm::CatalogRegistry,
    detailed: Option<&Vec<(String, String, heycode_core::ProviderResponseMetadata)>>,
    reported_steps: usize,
) -> String {
    let cache_aware = detailed.is_some_and(|rows| {
        rows.iter()
            .any(|(_, _, metadata)| metadata.cache_usage().is_some())
    });
    match session_cost(usage, models, detailed, reported_steps) {
        Ok((pico_units, currency)) => format!(
            "derived cost{}{}: {pico_units} pico-{}",
            if usage.is_complete() { "" } else { " at least" },
            if cache_aware { " (cache-aware)" } else { "" },
            currency.code()
        ),
        Err(reason) => format!("derived cost: unknown ({reason})"),
    }
}

pub(crate) fn descriptor(
    id: &'static str,
    description: &'static str,
    source: CommandSource,
) -> Result<CommandDescriptor, heycode_agent::CommandMetadataError> {
    CommandDescriptor::new(
        id,
        description,
        Vec::new(),
        CommandTiming::Immediate,
        source,
    )
}

pub(crate) fn require_no_args(args: &str, command: &'static str) -> anyhow::Result<()> {
    if args.trim().is_empty() {
        Ok(())
    } else {
        anyhow::bail!("usage: /{command}")
    }
}

struct StatusCommand {
    descriptor: CommandDescriptor,
    shell: Arc<SettingsShellSnapshotService>,
}

#[async_trait]
impl Command for StatusCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "status")?;
        self.shell
            .request(agent, SettingsShellTab::Status, true)
            .await;
        Ok(())
    }
}

struct DoctorCommand {
    descriptor: CommandDescriptor,
    state: Arc<StatusState>,
}

#[async_trait]
impl Command for DoctorCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "doctor")?;
        let report = self.state.doctor_report().await?;
        agent.ui().emit(UiEvent::Info {
            text: report.render_human(),
        });
        Ok(())
    }
}

struct PermissionCommand {
    descriptor: CommandDescriptor,
    state: Arc<StatusState>,
}

#[async_trait]
impl Command for PermissionCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let requested = args.trim();
        if !requested.is_empty() {
            if requested == "plan" {
                agent.enter_plan().await?;
                return Ok(());
            }
            let mode = match requested {
                "ask" | "default" => heycode_agent::ApprovalPolicyKind::Ask,
                "full" | "full_access" => heycode_agent::ApprovalPolicyKind::FullAccess,
                "accepted" | "accepted_edits" => heycode_agent::ApprovalPolicyKind::AcceptedEdits,
                "deny" => heycode_agent::ApprovalPolicyKind::Deny,
                // The argument is never echoed: it is user input and may be
                // anything.
                _ => anyhow::bail!("Choose full_access, accepted_edits, default or plan"),
            };
            switch_permission_mode(&self.state, agent, mode)?;
            return Ok(());
        }
        agent.ui().emit(UiEvent::PermissionPickerRequested {
            report: self.state.sandbox_report(),
        });
        Ok(())
    }
}

fn switch_permission_mode(
    state: &StatusState,
    agent: &heycode_agent::Agent,
    mode: heycode_agent::ApprovalPolicyKind,
) -> anyhow::Result<()> {
    let Some(switch) = state.approval_switch.as_ref() else {
        anyhow::bail!(
            "the approval mode is fixed on this surface; set `approval.mode` in config or pass --approval"
        );
    };
    switch.switch_by_user(mode).map_err(anyhow::Error::msg)?;
    agent.ui().emit(UiEvent::PermissionModeChanged { mode });
    agent.ui().emit(UiEvent::Info {
        text: format!("Permissions: {}. {}", mode.label(), mode.description()),
    });
    Ok(())
}

struct SandboxCommand {
    descriptor: CommandDescriptor,
    state: Arc<StatusState>,
}

struct WebStatusCommand {
    descriptor: CommandDescriptor,
    web: Arc<heycode_web::WebRegistry>,
}

#[async_trait]
impl Command for WebStatusCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "web")?;
        let report = self.web.capability_report()?;
        agent.ui().emit(UiEvent::Info {
            text: render_web(&report),
        });
        Ok(())
    }
}

#[async_trait]
impl Command for SandboxCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "sandbox")?;
        let report = self.state.sandbox_report();
        agent.ui().emit(UiEvent::SandboxPanelRequested { report });
        Ok(())
    }
}

fn render_status(
    agent: &heycode_agent::Agent,
    doctor: Option<&DoctorReport>,
    sandbox: &SandboxCapabilityReport,
    instructions: &[String],
) -> String {
    let selection = agent.selection();
    let current = sandbox.choice(sandbox.effective_mode);
    let network = current.map_or("unspecified", |choice| network_label(choice.network));
    [
        "status".to_owned(),
        format!("runtime: {}", agent.runtime_id()),
        format!("provider: {}", selection.provider_name),
        format!("model: {}", selection.model),
        format!("workspace: {}", agent.cwd().display()),
        format!(
            "project instructions: {}",
            if instructions.is_empty() {
                "none".to_owned()
            } else {
                instructions.join(", ")
            }
        ),
        format!("permission: {}", agent.approval_kind().as_str()),
        format!("sandbox: {}", sandbox.effective_mode.as_str()),
        format!(
            "active backend: {}",
            sandbox.active_backend.unwrap_or("none")
        ),
        format!(
            "available backend: {}",
            sandbox.available_backend.unwrap_or("none")
        ),
        format!("network: {network}"),
        doctor.map_or_else(
            || "doctor: not refreshed (run /status)".to_owned(),
            |doctor| {
                format!(
                    "doctor: {} ({} passed, {} warnings, {} failed, {} skipped)",
                    if doctor.healthy {
                        "healthy"
                    } else {
                        "unhealthy"
                    },
                    doctor.summary.passed,
                    doctor.summary.warnings,
                    doctor.summary.failed,
                    doctor.summary.skipped,
                )
            },
        ),
    ]
    .join("\n")
}

fn render_web(report: &heycode_web::WebCapabilityReport) -> String {
    let mut lines = vec![
        "web".to_owned(),
        format!("web search: {}", render_web_selection(&report.search)),
        format!("web fetch: {}", render_web_selection(&report.fetch)),
        format!(
            "web search domains: {}",
            render_domain_policy(&report.search_domains)
        ),
        format!(
            "web fetch domains: {}",
            render_domain_policy(&report.fetch_domains)
        ),
    ];
    for provider in &report.providers {
        let mut operations = Vec::new();
        if provider.descriptor.supports_search() {
            operations.push("search");
        }
        if provider.descriptor.supports_fetch() {
            operations.push("fetch");
        }
        lines.push(format!(
            "web provider {}: {} · {}",
            provider.descriptor.id(),
            operations.join("+"),
            if provider.available {
                "available"
            } else {
                "unavailable"
            }
        ));
    }
    for processor in &report.processors {
        lines.push(format!("web processor {}: available", processor.id()));
    }
    lines.join("\n")
}

fn render_web_selection(selection: &heycode_web::WebProviderSelection) -> String {
    match selection {
        heycode_web::WebProviderSelection::Selected {
            provider,
            configured,
        } => format!(
            "{provider} ({})",
            if *configured {
                "configured"
            } else {
                "automatic"
            }
        ),
        heycode_web::WebProviderSelection::Unavailable => "unavailable".to_owned(),
        heycode_web::WebProviderSelection::ConfiguredMissing { provider } => {
            format!("configured missing: {provider}")
        }
        heycode_web::WebProviderSelection::ConfiguredUnsupported { provider } => {
            format!("configured unsupported: {provider}")
        }
        heycode_web::WebProviderSelection::ConfiguredUnavailable { provider } => {
            format!("configured unavailable: {provider}")
        }
        heycode_web::WebProviderSelection::Ambiguous { providers } => {
            format!("ambiguous: {}", providers.join(","))
        }
    }
}

fn render_domain_policy(policy: &heycode_web::WebDomainPolicy) -> String {
    format!(
        "allow={} · block={}",
        render_domain_list(policy.allow(), "any"),
        render_domain_list(policy.block(), "none")
    )
}

fn render_domain_list(domains: &[String], empty: &str) -> String {
    if domains.is_empty() {
        return empty.to_owned();
    }
    let mut rendered = domains
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    if domains.len() > 8 {
        rendered.push_str(&format!(" (+{})", domains.len() - 8));
    }
    rendered
}

const fn network_label(scope: NetworkScope) -> &'static str {
    match scope {
        NetworkScope::Host => "host (not isolated)",
        NetworkScope::Isolated => "isolated or denied",
        NetworkScope::Unspecified => "guarantee unavailable",
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn stats_states_keep_empty_unavailable_and_failed_distinct() {
        assert!(matches!(
            stats_section(Ok(heycode_session::SessionStatsSnapshot::default())).0,
            SettingsShellSection::Empty { .. }
        ));
        assert!(matches!(
            stats_section(Err(
                heycode_session::SessionQueryError::StatisticsUnavailable
            ))
            .0,
            SettingsShellSection::Unavailable { .. }
        ));
        assert!(matches!(
            stats_section(Err(heycode_session::SessionQueryError::StoreUnavailable)).0,
            SettingsShellSection::Failed { .. }
        ));
    }
}

#[cfg(test)]
mod context_and_cost_tests {
    use super::*;

    fn plain(window: Option<u64>, used: Option<u64>, categories: &[(&str, u64)]) -> Vec<String> {
        let categories = categories
            .iter()
            .map(|(label, tokens)| ContextCategory {
                label: (*label).to_owned(),
                tokens: *tokens,
            })
            .collect::<Vec<_>>();
        context_grid_lines(
            window,
            used,
            &categories,
            &["deepseek-v4-flash".to_owned(), "fake/deepseek".to_owned()],
            "Estimated usage by category",
        )
    }

    #[test]
    fn the_grid_is_ten_rows_of_twenty_cells() {
        let rows = plain(Some(1_000_000), Some(1_300), &[("Tool schemas", 1_300)]);
        assert_eq!(rows.len(), 10);
        for row in &rows {
            let grid = row.split("   ").next().unwrap_or_default();
            let cells = grid
                .chars()
                .filter(|glyph| *glyph == '⛀' || *glyph == '⛶')
                .count();
            assert_eq!(cells, 20, "row {row:?} is not twenty cells wide");
        }
    }

    #[test]
    fn a_small_but_real_category_still_occupies_one_cell() {
        let rows = plain(Some(1_000_000), Some(41), &[("System instructions", 41)]);
        assert!(rows[0].starts_with("⛀ ⛶"), "{:?}", rows[0]);
        assert!(rows[0].contains("deepseek-v4-flash"));
    }

    #[test]
    fn an_unmeasured_session_is_all_free_space_and_says_so() {
        let rows = plain(Some(1_000_000), None, &[]);
        assert!(rows.iter().all(|row| !row.contains('⛀')));
        assert!(
            rows.iter()
                .any(|row| row.contains("Free space: the whole window — nothing measured yet")),
            "{rows:?}"
        );
    }

    #[test]
    fn categories_are_listed_with_tokens_and_a_share_of_the_window() {
        let rows = plain(
            Some(1_000_000),
            Some(1_241),
            &[("System instructions", 41), ("Tool schemas", 1_200)],
        );
        let legend = rows.join("\n");
        assert!(
            legend.contains("⛁ System instructions: 41 tokens (0.0%)"),
            "{legend}"
        );
        assert!(
            legend.contains("⛁ Tool schemas: 1.2k tokens (0.1%)"),
            "{legend}"
        );
        assert!(legend.contains("⛶ Free space: 998.8k (99.9%)"), "{legend}");
    }

    #[test]
    fn compact_tokens_uses_the_reference_scale() {
        assert_eq!(compact_tokens(41), "41");
        assert_eq!(compact_tokens(1_200), "1.2k");
        assert_eq!(compact_tokens(998_759), "998.8k");
        assert_eq!(compact_tokens(1_048_576), "1.0m");
    }

    #[test]
    fn durations_round_to_the_reference_shape() {
        assert_eq!(compact_duration_ms(0), "0s");
        assert_eq!(compact_duration_ms(8_400), "8s");
        assert_eq!(compact_duration_ms(187_000), "3m 7s");
        assert_eq!(compact_duration_ms(7_260_000), "2h 1m");
    }

    #[test]
    fn recorded_code_changes_counts_only_stored_edit_diffs() {
        let call = heycode_core::CallId::from_raw("call-1");
        let events = vec![
            heycode_session::SessionEvent {
                v: 1,
                seq: 1,
                time_ms: 0,
                kind: heycode_session::SessionEventKind::ToolCall {
                    turn: 1,
                    call_id: call.clone(),
                    name: "edit".to_owned(),
                    args: serde_json::json!({"path": "a.rs"}),
                },
            },
            heycode_session::SessionEvent {
                v: 1,
                seq: 2,
                time_ms: 1,
                kind: heycode_session::SessionEventKind::ToolResult {
                    call_id: call,
                    content: serde_json::json!({
                        "diff": "--- a.rs\n+++ a.rs\n-old\n+new\n+extra\n context"
                    })
                    .to_string(),
                    is_error: false,
                    untrusted_content: None,
                },
            },
        ];
        assert_eq!(recorded_code_changes(&events), (2, 1));
    }

    #[test]
    fn an_unpriced_session_reports_unknown_rather_than_zero() {
        let usage = heycode_session::project_usage(&[]);
        let models = heycode_llm::CatalogRegistry::new(std::time::Duration::from_secs(60));
        let block = session_block(&[], &usage, &models, None, 0);
        assert_eq!(block[0], "Session");
        assert!(
            block
                .iter()
                .any(|row| row.starts_with("Total cost:") && row.contains("unknown")),
            "{block:?}"
        );
        assert!(
            block.iter().any(|row| row
                == "Usage:                 0 input, 0 output, 0 cache read, 0 cache write"),
            "{block:?}"
        );
    }
}

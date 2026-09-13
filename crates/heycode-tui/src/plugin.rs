//! The TUI plugin: publishes the interactive shell handle and built-in surfaces.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandAvailability, CommandDescriptor, CommandSource, CommandTiming,
};
use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_ui::{SERVICE_UI, UiContributionDescriptor, UiRegistry, UiSlot};

struct ProfileCommand {
    descriptor: CommandDescriptor,
    profiles: Arc<heycode_config::NamedProfileService>,
    empty: CommandAvailability,
    unavailable: CommandAvailability,
}

struct SettingsCommand {
    descriptor: CommandDescriptor,
    shell: Option<Arc<heycode_status::SettingsShellSnapshotService>>,
    bridge: crate::panel_commands::PanelCommandBridge,
    detached: CommandAvailability,
}

#[async_trait]
impl Command for SettingsCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.shell.is_some()
            || self
                .bridge
                .is_attached(crate::panel_commands::CapabilityPanel::Settings)
        {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        if !args.trim().is_empty() {
            anyhow::bail!("usage: /settings");
        }
        if let Some(shell) = &self.shell {
            shell.request_config(agent).await;
        } else {
            self.bridge
                .request(crate::panel_commands::CapabilityPanel::Settings);
        }
        Ok(())
    }
}

#[async_trait]
impl Command for ProfileCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        match self.profiles.list() {
            Ok(rows) if rows.is_empty() => self.empty.clone(),
            Ok(_) => CommandAvailability::available(),
            Err(_) => self.unavailable.clone(),
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let name = args.trim();
        if name.is_empty() {
            agent
                .ui()
                .emit(heycode_agent::UiEvent::ProfilePickerRequested);
            return Ok(());
        }
        self.profiles
            .load(name)
            .map_err(|_| anyhow::anyhow!("named profile `{name}` is unavailable"))?;
        agent.ui().emit(heycode_agent::UiEvent::ProfileSelected {
            name: name.to_owned(),
        });
        Ok(())
    }
}

/// Handle the CLI awaits to run the interactive session.
///
/// Carries the shell's panel-open inbox so a command plugin can reach the
/// surfaces this crate owns without the composition root learning what a
/// panel is. A default handle has attached nothing, which is the honest
/// reading of a shell whose capability services were never wired.
#[derive(Clone, Default)]
pub struct TuiHandle {
    panels: crate::panel_commands::PanelCommandBridge,
    human_commands: crate::human_commands::HumanCommandBridge,
    advisor_bridge: crate::advisor_panel::AdvisorPanelBridge,
    advisor_control: Option<(
        Arc<heycode_agent::AdvisorService>,
        Arc<heycode_agent::Agent>,
    )>,
    recomposition: crate::recomposition::RecompositionBridge,
    add_directory: Option<crate::add_directory::AddDirectoryBridge>,
    session_commands: crate::session_browser::SessionCommandBridge,
    session_query: Option<Arc<heycode_session::SessionQueryService>>,
    ui: Option<Arc<heycode_ui::UiRegistry>>,
    session_events: crate::transcript::SessionEventBridge,
    execution_jobs: Option<Arc<heycode_agent::ExecutionJobService>>,
    workflows: Option<Arc<heycode_agent::WorkflowService>>,
    mcp_client: Option<Arc<crate::product_attachments::McpTuiBridge>>,
}

impl TuiHandle {
    /// The shell's panel-open inbox, shared rather than copied.
    #[must_use]
    pub fn panels(&self) -> crate::panel_commands::PanelCommandBridge {
        self.panels.clone()
    }

    /// Human-only CMD09/CMD10 request inbox shared with the running shell.
    #[must_use]
    pub fn human_commands(&self) -> crate::human_commands::HumanCommandBridge {
        self.human_commands.clone()
    }

    /// Session lifecycle command inbox shared with the running shell.
    #[must_use]
    pub fn session_commands(&self) -> crate::session_browser::SessionCommandBridge {
        self.session_commands.clone()
    }

    /// Run with automatic terminal-capability detection until quit.
    ///
    /// # Errors
    /// Terminal setup/teardown and loop failures.
    ///
    /// Returns a typed exit or workspace-trust recomposition request after
    /// terminal state has been restored.
    pub async fn run(
        &self,
        deps: crate::app::LoopDeps,
    ) -> anyhow::Result<crate::app::TuiRunOutcome> {
        self.run_with_display(deps, crate::terminal::TuiDisplayMode::Automatic)
            .await
    }

    /// Run the lower/product-neutral screen-reader boundary.
    ///
    /// The composition root must explicitly route a product flag to this
    /// method; this crate does not inspect a hidden environment variable.
    ///
    /// # Errors
    /// Terminal setup/teardown, flat-output, and loop failures.
    pub async fn run_screen_reader(
        &self,
        deps: crate::app::LoopDeps,
    ) -> anyhow::Result<crate::app::TuiRunOutcome> {
        self.run_with_display(deps, crate::terminal::TuiDisplayMode::ScreenReader)
            .await
    }

    /// Run with an explicit product-neutral presentation choice.
    ///
    /// # Errors
    /// Terminal setup/teardown, rendering, and loop failures.
    pub async fn run_with_display(
        &self,
        deps: crate::app::LoopDeps,
        display: crate::terminal::TuiDisplayMode,
    ) -> anyhow::Result<crate::app::TuiRunOutcome> {
        let _recomposition_attachment = self.recomposition.attach(display)?;
        let _add_directory_attachment = self
            .add_directory
            .as_ref()
            .map(|bridge| bridge.attach())
            .transpose()?;
        let environment = crate::terminal::detect_environment();
        let capabilities = display.resolve(&environment);
        // `--screen-reader` draws flat because the user asked, not because the
        // terminal is incapable. Such a terminal can still report a bracketed
        // paste, and a screen-reader user pastes multi-line text like anyone
        // else.
        let terminal_is_flat = heycode_ui::terminal::TerminalCapabilities::detect(&environment)
            .render()
            == heycode_ui::terminal::RenderMode::Flat;
        let chrome =
            crate::terminal::Chrome::new(capabilities.render()).with_input(if terminal_is_flat {
                crate::terminal::InputProtocol::PlainKeys
            } else {
                crate::terminal::InputProtocol::Escapes
            });
        crossterm::terminal::enable_raw_mode()?;
        let _raw_mode = RawModeGuard;
        let _screen = TerminalScreenGuard::enter(chrome)?;

        if chrome.alternate_screen() {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
            let mut draw_error = None;
            let result = self
                .run_loop(deps, capabilities, chrome.input(), |state| {
                    if draw_error.is_none()
                        && let Err(error) = terminal.draw(|frame| crate::render::draw(frame, state))
                    {
                        draw_error = Some(error);
                        state.quit_requested = true;
                    }
                })
                .await;
            if let Some(error) = draw_error {
                return Err(anyhow::anyhow!("terminal draw failed: {error}"));
            }
            return result;
        }

        let mut output = crate::app::accessibility::FlatOutput::new(std::io::stdout());
        let mut draw_error = None;
        let result = self
            .run_loop(deps, capabilities, chrome.input(), |state| {
                state.refresh_tool_groups();
                if draw_error.is_none()
                    && let Err(error) = output.render(state)
                {
                    draw_error = Some(error);
                    state.quit_requested = true;
                }
            })
            .await;
        if let Some(error) = draw_error {
            return Err(anyhow::anyhow!("flat terminal output failed: {error}"));
        }
        result
    }

    async fn run_loop<D>(
        &self,
        deps: crate::app::LoopDeps,
        capabilities: heycode_ui::terminal::TerminalCapabilities,
        input: crate::terminal::InputProtocol,
        draw_fn: D,
    ) -> anyhow::Result<crate::app::TuiRunOutcome>
    // `input` records what the entered screen asked of the terminal, which
    // capabilities alone cannot say for a flat frame the user requested.
    where
        D: FnMut(&mut crate::app::AppState),
    {
        let startup_preferences = match self.ui.as_ref() {
            Some(ui) => {
                let preferences = heycode_ui::preferences::SettingsBackedUiPreferences::new(
                    deps.settings.clone(),
                )
                .load()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let theme = ui
                    .theme(preferences.preferences.theme_id())
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "selected theme `{}` is unavailable",
                            preferences.preferences.theme_id()
                        )
                    })?;
                let keymap = heycode_ui::keymap::SettingsBackedKeymap::new(deps.settings.clone())
                    .load()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                Some(crate::app::StartupUiPreferences {
                    theme,
                    keymap,
                    vim_mode: preferences.preferences.vim_mode(),
                    focus_view: preferences.preferences.focus_view(),
                    shell: preferences.preferences.shell(),
                    scroll_speed: preferences.preferences.scroll_speed(),
                })
            }
            None => None,
        };
        match self.session_query.clone() {
            Some(session_query) => {
                crate::app::run_interactive_with_sessions_and_capabilities(
                    deps,
                    crate::app::InteractiveSurfaces {
                        panels: self.panels.clone(),
                        human_commands: self.human_commands.clone(),
                        advisor_bridge: self.advisor_bridge.clone(),
                        advisor_control: self.advisor_control.clone(),
                        recomposition: self.recomposition.clone(),
                        add_directory: self.add_directory.clone(),
                        startup_preferences,
                        session_events: self.session_events.clone(),
                        execution_jobs: self.execution_jobs.clone(),
                        workflows: self.workflows.clone(),
                        mcp_client: self.mcp_client.clone(),
                        input,
                    },
                    session_query,
                    self.session_commands.clone(),
                    capabilities,
                    draw_fn,
                )
                .await
            }
            None => {
                crate::app::run_interactive_with_capabilities(
                    deps,
                    crate::app::InteractiveSurfaces {
                        panels: self.panels.clone(),
                        human_commands: self.human_commands.clone(),
                        advisor_bridge: self.advisor_bridge.clone(),
                        advisor_control: self.advisor_control.clone(),
                        recomposition: self.recomposition.clone(),
                        add_directory: self.add_directory.clone(),
                        startup_preferences,
                        session_events: self.session_events.clone(),
                        execution_jobs: self.execution_jobs.clone(),
                        workflows: self.workflows.clone(),
                        mcp_client: self.mcp_client.clone(),
                        input,
                    },
                    capabilities,
                    draw_fn,
                )
                .await
            }
        }
    }
}

struct TerminalScreenGuard {
    chrome: crate::terminal::Chrome,
}

impl TerminalScreenGuard {
    fn enter(chrome: crate::terminal::Chrome) -> std::io::Result<Self> {
        // Arm restoration before setup: a partial write can already enable
        // mouse reporting even when entering the screen subsequently fails.
        let guard = Self { chrome };
        crate::terminal::enter_terminal_screen(&mut std::io::stdout(), chrome)?;
        Ok(guard)
    }
}

impl Drop for TerminalScreenGuard {
    fn drop(&mut self) {
        let mut bytes = Vec::new();
        let _ = crate::terminal::leave_terminal_screen(&mut bytes, self.chrome);
        let _ = crate::terminal::restore_terminal_output(&bytes);
    }
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = nix::sys::termios::tcflush(std::io::stdin(), nix::sys::termios::FlushArg::TCIFLUSH);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Provide service `"tui"`.
#[must_use]
pub fn tui_plugin() -> Box<dyn Plugin> {
    build_tui_plugin(None)
}

/// Provide service `"tui"` with the session-scoped MCP11 human bridge.
#[must_use]
pub fn tui_plugin_with_mcp_bridge(
    bridge: Arc<crate::product_attachments::McpTuiBridge>,
) -> Box<dyn Plugin> {
    build_tui_plugin(Some(bridge))
}

fn build_tui_plugin(
    mcp_client: Option<Arc<crate::product_attachments::McpTuiBridge>>,
) -> Box<dyn Plugin> {
    struct TranscriptPanel;
    struct McpPanel;
    struct PluginPanel;
    struct SkillsPanel;
    struct AgentsPanel;
    struct HooksPanel;
    struct SessionPanel;
    struct ApprovalDialog;
    struct SessionStatus;
    struct TuiPlugin(Option<Arc<crate::product_attachments::McpTuiBridge>>);
    impl Plugin for TuiPlugin {
        fn name(&self) -> &'static str {
            "tui"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "tui",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::UserInterface,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::UserInterface,
                    "tui",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "profile",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    "keymap",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    "ui-preferences",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "diff",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "copy",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "mention",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "focus",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "color",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "advisor",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "review",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "ask-advisor",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "security-review",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "theme",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "keymap",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "vim",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "scroll-speed",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "statusline",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "release-notes",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "insights",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "reload-plugins",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "tui",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "settings",
                ),
                #[cfg(unix)]
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "background",
                ),
                #[cfg(unix)]
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "fork",
                ),
                #[cfg(unix)]
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "sessions",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "new",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "resume",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "branch",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "rename",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "archive",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "delete",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "export",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "rewind",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "voice",
                ),
            ]
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_TUI]
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                SERVICE_UI,
                heycode_agent::SERVICE_COMMANDS,
                heycode_app_server::SERVICE_APP_SERVER,
                heycode_config::SERVICE_PROFILES,
                heycode_settings::SERVICE_SETTINGS,
                heycode_ui::SERVICE_SETTINGS_UI,
                heycode_session::SERVICE_SESSION,
                heycode_session::SERVICE_SESSION_QUERY,
                heycode_runtime::SERVICE_RUNTIMES,
                heycode_routing::SERVICE_ROUTING,
            ]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let ui = ctx
                .get::<UiRegistry>(SERVICE_UI)
                .ok_or_else(|| CoreError::other("ui registry missing"))?;
            ui.register(
                ctx,
                UiContributionDescriptor::new(
                    UiSlot::Panel,
                    "transcript",
                    "Conversation transcript",
                    100,
                )
                .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(TranscriptPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            for kind in [
                crate::side_panel::SidePanelKind::Diff,
                crate::side_panel::SidePanelKind::Jobs,
                crate::side_panel::SidePanelKind::Agents,
            ] {
                ui.register(
                    ctx,
                    crate::side_panel::descriptor(kind.as_str(), kind.title())
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    Arc::new(crate::side_panel::SidePanelContribution::new(kind)),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            }
            ui.register(
                ctx,
                crate::mcp_panel::panel_descriptor()
                    .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(McpPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                crate::plugin_panel::panel_descriptor()
                    .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(PluginPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                crate::panel_commands::catalog_panel_descriptor(
                    crate::panel_commands::CapabilityPanel::Skills,
                )
                .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(SkillsPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                crate::panel_commands::catalog_panel_descriptor(
                    crate::panel_commands::CapabilityPanel::Agents,
                )
                .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(AgentsPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                crate::panel_commands::catalog_panel_descriptor(
                    crate::panel_commands::CapabilityPanel::Hooks,
                )
                .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(HooksPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                crate::session_browser::panel_descriptor()
                    .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(SessionPanel),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                UiContributionDescriptor::new(UiSlot::Dialog, "approval", "Tool approval", 100)
                    .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(ApprovalDialog),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            ui.register(
                ctx,
                UiContributionDescriptor::new(UiSlot::Status, "session", "Session status", 100)
                    .map_err(|error| CoreError::other(error.to_string()))?,
                Arc::new(SessionStatus),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let commands = ctx
                .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("command registry missing"))?;
            let profiles = ctx
                .get::<heycode_config::NamedProfileService>(heycode_config::SERVICE_PROFILES)
                .ok_or_else(|| CoreError::other("named profile service missing"))?;
            let settings = ctx
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service missing"))?;
            settings
                .register(
                    ctx,
                    heycode_ui::keymap::settings_definition()
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            settings
                .register(
                    ctx,
                    heycode_ui::preferences::settings_definition()
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.get::<heycode_ui::settings_ui::SettingsUiRegistry>(heycode_ui::SERVICE_SETTINGS_UI)
                .ok_or_else(|| CoreError::other("settings UI registry missing"))?;
            let session_query = ctx
                .get::<heycode_session::SessionQueryService>(heycode_session::SERVICE_SESSION_QUERY)
                .ok_or_else(|| CoreError::other("session query service missing"))?;
            let session = ctx
                .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session service missing"))?;
            let session_bus = session
                .lock()
                .map_err(|_| CoreError::other("session unavailable"))?
                .bus();
            let session_events = crate::transcript::SessionEventBridge::default();
            let listener = session_events.clone();
            session_bus.on_effect::<heycode_session::SessionEvent>(ctx, move |event| {
                listener.publish(event);
            });
            let handle = TuiHandle {
                panels: crate::panel_commands::PanelCommandBridge::new(),
                human_commands: crate::human_commands::HumanCommandBridge::new(),
                advisor_bridge: crate::advisor_panel::AdvisorPanelBridge::new(),
                advisor_control: ctx
                    .get::<heycode_agent::AdvisorService>(heycode_agent::SERVICE_ADVISOR)
                    .zip(ctx.get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)),
                recomposition: crate::recomposition::RecompositionBridge::default(),
                add_directory: ctx
                    .get::<crate::add_directory::AddDirectoryBridge>(
                        crate::add_directory::SERVICE_ADD_DIRECTORY_PROMPT,
                    )
                    .map(|bridge| (*bridge).clone()),
                session_commands: crate::session_browser::SessionCommandBridge::new(),
                session_query: Some(session_query.clone()),
                ui: Some(ui.clone()),
                session_events,
                execution_jobs: ctx.get::<heycode_agent::ExecutionJobService>(
                    heycode_agent::SERVICE_EXECUTION_JOBS,
                ),
                workflows: ctx
                    .get::<heycode_agent::WorkflowService>(heycode_agent::SERVICE_WORKFLOWS),
                mcp_client: self.0.clone(),
            };
            let settings_bridge = handle.panels();
            let settings_shell = ctx.get::<heycode_status::SettingsShellSnapshotService>(
                heycode_status::SERVICE_SETTINGS_SHELL,
            );
            let human_bridge = handle.human_commands();
            let shell_bridge = human_bridge.clone();
            let shell_settings = settings.clone();
            settings
                .watch(
                    ctx,
                    &heycode_ui::preferences::settings_namespace()
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    move |_| {
                        if let Ok(preferences) =
                            heycode_ui::preferences::SettingsBackedUiPreferences::new(
                                shell_settings.clone(),
                            )
                            .load()
                        {
                            shell_bridge.request(
                                crate::human_commands::HumanCommandRequest::ApplyShell(
                                    preferences.preferences.shell(),
                                ),
                            );
                            shell_bridge.request(
                                crate::human_commands::HumanCommandRequest::ApplyScrollSpeed {
                                    quarters: (preferences.preferences.scroll_speed() * 4.0) as u8,
                                },
                            );
                        }
                    },
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let source = CommandSource::from_plugin(self.name())
                .map_err(|error| CoreError::other(error.to_string()))?;
            let descriptor = CommandDescriptor::new(
                "profile",
                "Choose a named profile and recompose",
                vec![
                    heycode_agent::CommandArgument::optional("name", "Named profile file stem")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                ],
                CommandTiming::Queued,
                source.clone(),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let empty = CommandAvailability::unavailable(
                "No named profiles exist under the configured heycode home",
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let unavailable = CommandAvailability::unavailable("Named profiles are unavailable")
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(ProfileCommand {
                        descriptor,
                        profiles,
                        empty,
                        unavailable,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            for command in crate::human_commands::commands_with_advisor(
                source.clone(),
                human_bridge,
                settings,
                ui,
                // Native inspection is optional in intentionally minimal profiles.
                ctx.get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS),
                handle
                    .advisor_control
                    .as_ref()
                    .map(|(service, _)| service.clone()),
                handle.advisor_bridge.clone(),
            )
            .map_err(|error| CoreError::other(error.to_string()))?
            {
                commands
                    .register_effect(ctx, command)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            commands
                .register_effect(
                    ctx,
                    crate::release_notes::command(source.clone())
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    crate::insights::command(source.clone(), session_query.clone())
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            for command in
                crate::recomposition::commands(source.clone(), handle.recomposition.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?
            {
                commands
                    .register_effect(ctx, command)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            let settings_descriptor = CommandDescriptor::new(
                "settings",
                "Browse and edit plugin settings",
                Vec::new(),
                CommandTiming::Immediate,
                CommandSource::from_plugin(self.name())
                    .map_err(|error| CoreError::other(error.to_string()))?,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let settings_detached =
                CommandAvailability::unavailable("Settings are not attached to this shell")
                    .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(SettingsCommand {
                        descriptor: settings_descriptor,
                        shell: settings_shell,
                        bridge: settings_bridge,
                        detached: settings_detached,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let session_source = CommandSource::from_plugin(self.name())
                .map_err(|error| CoreError::other(error.to_string()))?;
            #[cfg(unix)]
            for command in crate::app::session_background::commands(
                session_source.clone(),
                handle.session_commands(),
                session.clone(),
            )
            .map_err(|error| CoreError::other(error.to_string()))?
            {
                commands
                    .register_effect(ctx, command)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            for command in
                crate::session_browser::session_commands(session_source, handle.session_commands())
                    .map_err(|error| CoreError::other(error.to_string()))?
            {
                commands
                    .register_effect(ctx, command)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            commands
                .register_effect(
                    ctx,
                    crate::session_control::rewind_command(handle.session_commands())
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    crate::voice::command().map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.provide(crate::SERVICE_TUI, "tui", handle)
                .map_err(|e| CoreError::other(e.to_string()))
        }
    }
    Box::new(TuiPlugin(mcp_client))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn tui_declares_session_query_and_all_lifecycle_commands() {
        let plugin = tui_plugin();
        assert!(
            plugin
                .inject()
                .contains(&heycode_session::SERVICE_SESSION_QUERY)
        );
        assert!(plugin.inject().contains(&heycode_session::SERVICE_SESSION));
        assert!(!plugin.inject().contains(&heycode_agent::SERVICE_SUBAGENTS));
        let commands = plugin
            .inventory()
            .into_iter()
            .filter(|row| row.kind == heycode_core::ContributionKind::Command)
            .map(|row| row.name)
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            [
                "profile",
                "diff",
                "copy",
                "mention",
                "focus",
                "color",
                "advisor",
                "review",
                "ask-advisor",
                "security-review",
                "theme",
                "keymap",
                "vim",
                "scroll-speed",
                "statusline",
                "release-notes",
                "insights",
                "reload-plugins",
                "tui",
                "settings",
                #[cfg(unix)]
                "background",
                #[cfg(unix)]
                "fork",
                #[cfg(unix)]
                "sessions",
                "new",
                "resume",
                "branch",
                "rename",
                "archive",
                "delete",
                "export",
                "rewind",
                "voice",
            ]
        );
    }
}

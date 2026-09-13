//! CMD04 capability-panel command contracts: registration, attribution,
//! disposal, honest availability and the exact panel each command opens.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_agent::{AgentOptions, DenyAll};
use heycode_core::{Context, Plugin, compose};
use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginState, PluginStateStore,
};
use heycode_extensions::{PluginId, PluginVersion};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, Provider, llm_plugin};
use heycode_mcp::McpTransportKind;
use heycode_mcp::management::{
    McpDefinitionStore, McpManagement, McpManagementError, StoredServer,
};
use heycode_tui::app::AppState;
use heycode_tui::panel_commands::{CapabilityPanel, PanelCommandBridge, panel_commands_plugin};
use heycode_tui::render::draw;
use heycode_tui::{SERVICE_TUI, TuiHandle};
use ratatui::{Terminal, backend::TestBackend};

#[derive(Default)]
struct ServerStore {
    servers: Mutex<BTreeMap<String, StoredServer>>,
}

impl McpDefinitionStore for ServerStore {
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        Ok(self.servers.lock().unwrap().clone())
    }

    fn persist(&self, servers: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> {
        *self.servers.lock().unwrap() = servers.clone();
        Ok(())
    }
}

fn mcp_fixture() -> (Arc<McpManagement>, Arc<ServerStore>) {
    let store = Arc::new(ServerStore::default());
    store
        .persist(&BTreeMap::from([(
            "example".to_owned(),
            StoredServer::new(
                "example",
                McpTransportKind::Stdio,
                "/usr/bin/example --serve",
            )
            .unwrap(),
        )]))
        .unwrap();
    (Arc::new(McpManagement::new(store.clone())), store)
}

fn mcp_management() -> Arc<McpManagement> {
    mcp_fixture().0
}

#[derive(Default)]
struct StateStore {
    states: Mutex<BTreeMap<String, PluginState>>,
}

impl PluginStateStore for StateStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.states.lock().unwrap().clone())
    }

    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.states.lock().unwrap() = states.clone();
        Ok(())
    }
}

struct NoVersions;

impl InstalledVersions for NoVersions {
    fn versions(&self, _id: &PluginId) -> Vec<PluginVersion> {
        Vec::new()
    }
}

fn plugin_lifecycle() -> Arc<PluginLifecycle> {
    Arc::new(PluginLifecycle::new(
        Arc::new(StateStore::default()),
        Arc::new(NoVersions),
    ))
}

/// Stand-in for the composed shell: publishes exactly the service the panel
/// command plugin injects, holding the inbox the test then inspects.
fn shell_plugin(handle: TuiHandle) -> Box<dyn Plugin> {
    struct ShellPlugin(Mutex<Option<TuiHandle>>);
    impl Plugin for ShellPlugin {
        fn name(&self) -> &'static str {
            "test-shell"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::unclassified(self.name())
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_TUI]
        }

        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            let handle = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .ok_or_else(|| heycode_core::CoreError::other("shell applied twice"))?;
            context.provide(SERVICE_TUI, "test-shell", handle)
        }
    }
    Box::new(ShellPlugin(Mutex::new(Some(handle))))
}

struct World {
    commands: Arc<heycode_agent::CommandRegistry>,
    bridge: PanelCommandBridge,
    context: Context,
}

fn command_world() -> World {
    let handle = TuiHandle::default();
    let bridge = handle.panels();
    let context = compose(&[
        heycode_agent::commands_plugin(),
        shell_plugin(handle),
        panel_commands_plugin(),
    ])
    .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    World {
        commands,
        bridge,
        context,
    }
}

pub(super) struct AgentWorld {
    pub(super) agent: Arc<heycode_agent::Agent>,
    commands: Arc<heycode_agent::CommandRegistry>,
    bridge: PanelCommandBridge,
    _context: Context,
    _root: tempfile::TempDir,
}

pub(super) fn agent_world() -> AgentWorld {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().canonicalize().unwrap();
    let handle = TuiHandle::default();
    let bridge = handle.panels();
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(Vec::new()));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(cwd.clone()),
        heycode_prompt::prompt_plugin(),
        // `tools` injects filesystem, shell and native-tools; the agent in turn
        // injects `tools`. Composition fails loud when any is absent (AGENTS.md
        // §1.4), which is what this list was missing.
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_hooks::hooks_plugin(heycode_trust::WorkspaceTrustDecision::Trusted),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        // Web stays off: these cases exercise slash commands opening panels,
        // and enabling it would pull `heycode-web` in as a dependency purely to
        // satisfy an inject nothing here uses.
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".to_owned(),
                model: "model-live".to_owned(),
            },
            vec![provider],
        ),
        heycode_agent::approval_plugin(Arc::new(DenyAll)),
        heycode_agent::agent_options_plugin(AgentOptions {
            cwd: Some(cwd),
            ..AgentOptions::default()
        }),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::agent_plugin(),
        shell_plugin(handle),
        panel_commands_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    AgentWorld {
        agent,
        commands,
        bridge,
        _context: context,
        _root: root,
    }
}

#[test]
fn the_plugin_registers_slash_mcp_as_an_immediate_command_it_attributes_and_disposes() {
    let mut world = command_world();
    let command = world.commands.get("mcp").unwrap().expect("/mcp registered");
    assert_eq!(command.descriptor().source().plugin(), "panel-commands");
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::Immediate
    );
    assert_eq!(command.descriptor().synopsis(), "/mcp [action] [server]");
    assert_eq!(
        command.descriptor().description(),
        "Open or manage MCP servers"
    );

    let snapshot = world.context.plugin_inventory().snapshot().unwrap();
    assert_eq!(
        snapshot
            .contributions
            .iter()
            .filter(|row| row.plugin == "panel-commands")
            .map(|row| (row.kind, row.name.as_str()))
            .collect::<Vec<_>>(),
        [
            (heycode_core::ContributionKind::Command, "mcp"),
            (heycode_core::ContributionKind::Command, "agents"),
            (heycode_core::ContributionKind::Command, "hooks"),
            (heycode_core::ContributionKind::Command, "workflows"),
            (heycode_core::ContributionKind::Command, "list-agents"),
            (heycode_core::ContributionKind::Command, "subtask"),
            (heycode_core::ContributionKind::Command, "schedule"),
        ]
    );

    world.context.shutdown();
    assert!(world.commands.get("mcp").unwrap().is_none());
    // Disposal removes exactly the row it added; the built-ins registered by
    // another plugin are untouched by this plugin's unwind.
    assert!(world.commands.get("help").unwrap().is_some());
}

#[tokio::test]
async fn slash_subtask_is_visible_but_honestly_unavailable_without_native_background_jobs() {
    let world = agent_world();
    let command = world.commands.get("subtask").unwrap().unwrap();
    assert_eq!(command.descriptor().source().plugin(), "panel-commands");
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::ModelScheduling
    );
    assert_eq!(command.descriptor().synopsis(), "/subtask <task...>");
    assert!(!command.availability().is_available());
    assert_eq!(
        command.availability().reason(),
        Some("Fork-capable native background subagents are not attached")
    );
    assert_eq!(
        command
            .execute(&world.agent, "")
            .await
            .unwrap_err()
            .to_string(),
        "usage: /subtask <task>"
    );
    assert!(
        command
            .execute(&world.agent, "inspect the parser")
            .await
            .unwrap_err()
            .to_string()
            .contains("not configured")
    );
    let list = world.commands.get("peers").unwrap().unwrap();
    assert_eq!(list.descriptor().id(), "list-agents");
    assert!(!list.availability().is_available());
    assert!(
        list.execute(&world.agent, "")
            .await
            .unwrap_err()
            .to_string()
            .contains("no subagent registry")
    );
    let schedule = world.commands.get("routines").unwrap().unwrap();
    assert_eq!(schedule.descriptor().id(), "schedule");
    assert!(!schedule.availability().is_available());
}

#[tokio::test]
async fn slash_subtask_admits_a_forked_native_background_job_and_returns_its_result() {
    use heycode_llm::{FinishReason, StreamChunk};

    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().canonicalize().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![vec![
        StreamChunk::TextDelta("background result".to_owned()),
        StreamChunk::Finish(FinishReason::Stop),
    ]])) as Arc<dyn Provider>;
    let handle = TuiHandle::default();
    let mut context = compose(&[
        heycode_session::session_plugin(cwd.join("sessions")),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(10),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..Default::default()
        }),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".to_owned(),
                model: "model-live".to_owned(),
            },
            vec![provider],
        ),
        heycode_agent::approval_plugin(Arc::new(DenyAll)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::subagent_plugin(cwd.join("sessions"), 3),
        heycode_agent::agent_options_plugin(AgentOptions {
            cwd: Some(cwd),
            ..AgentOptions::default()
        }),
        heycode_agent::agent_plugin(),
        heycode_agent::subagent_jobs_plugin(),
        heycode_agent::durable_schedules_plugin(),
        shell_plugin(handle),
        panel_commands_plugin(),
    ])
    .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let subtask = commands.get("subtask").unwrap().unwrap();
    assert!(subtask.availability().is_available());
    subtask
        .execute(&agent, "inspect the parser without blocking the parent")
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if registry
                .task_snapshots()
                .iter()
                .any(|task| task.state == heycode_agent::TaskState::Completed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let tasks = registry.task_snapshots();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].provider, "native");
    assert_eq!(tasks[0].output, "background result");
    assert!(tasks[0].session_id.is_some());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    agent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    let list = commands.get("list-agents").unwrap().unwrap();
    assert!(list.availability().is_available());
    assert_eq!(
        commands.get("peers").unwrap().unwrap().descriptor().id(),
        "list-agents"
    );
    list.execute(&agent, "").await.unwrap();
    assert!(seen.lock().unwrap().iter().any(|event| matches!(event,
        heycode_agent::UiEvent::Info { text }
            if text.contains("this session:")
                && text.contains("agents: 1")
                && !text.contains("background result")
                && text.contains("provider=native")
    )));
    let schedule = commands.get("routines").unwrap().unwrap();
    assert_eq!(schedule.descriptor().id(), "schedule");
    assert_eq!(
        schedule.descriptor().timing(),
        heycode_agent::CommandTiming::ModelScheduling
    );
    assert!(schedule.availability().is_available());
    schedule
        .execute(&agent, "after 60 check later")
        .await
        .unwrap();
    let schedules = context
        .get::<heycode_agent::DurableScheduleService>(heycode_agent::SERVICE_SCHEDULES)
        .unwrap();
    let active = schedules.list().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].prompt(), "check later");
    schedule.execute(&agent, "list").await.unwrap();
    schedule
        .execute(&agent, &format!("delete {}", active[0].id()))
        .await
        .unwrap();
    assert!(schedules.list().unwrap().is_empty());
    for invalid in ["after 0 no", "after x no", "at 1", "delete", "unknown 1 no"] {
        assert!(schedule.execute(&agent, invalid).await.is_err());
    }
    context.shutdown();
}

#[test]
fn the_plugin_refuses_a_world_without_the_shell_whose_inbox_it_writes_into() {
    let orphaned = compose(&[heycode_agent::commands_plugin(), panel_commands_plugin()]);
    assert!(
        orphaned.is_err(),
        "a command that fills an inbox nobody drains must not compose"
    );
}

#[test]
fn the_plugin_declares_both_services_it_reaches_into_so_composition_can_order_it() {
    // The declared graph is what orders composition and what the config
    // migration reads; an undeclared shell dependency would only be caught by
    // whichever world happened to apply the plugins in a lucky order.
    let plugin = panel_commands_plugin();
    assert_eq!(plugin.name(), "panel-commands");
    assert_eq!(
        plugin.inject(),
        [heycode_agent::SERVICE_COMMANDS, SERVICE_TUI]
    );
}

#[test]
fn slash_plugins_stays_owned_by_the_built_in_inventory_command() {
    // `/plugins` is registered by `heycode-agent`; CMD04 cannot re-point it at
    // the plugin panel without that crate releasing the id, and a second
    // registration would fail composition rather than shadow the owner.
    let world = command_world();
    let command = world
        .commands
        .get("plugins")
        .unwrap()
        .expect("/plugins registered");
    assert_eq!(command.descriptor().source().plugin(), "commands");
}

#[test]
fn slash_mcp_reports_why_it_cannot_run_until_the_shell_attaches_mcp_services() {
    let world = command_world();
    let command = world.commands.get("mcp").unwrap().unwrap();
    let detached = command.availability();
    assert!(!detached.is_available());
    assert_eq!(
        detached.reason(),
        Some("MCP management is not attached to this shell")
    );

    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_panel_commands(world.bridge.clone());
    assert!(!command.availability().is_available());

    state.set_mcp_services(mcp_management(), None);
    assert!(
        command.availability().is_available(),
        "attaching the operations layer is what makes /mcp runnable"
    );
    assert_eq!(command.availability().reason(), None);
}

#[test]
fn a_bridge_installed_after_the_services_still_reports_them_attached() {
    let world = command_world();
    let command = world.commands.get("mcp").unwrap().unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(mcp_management(), None);
    state.set_panel_commands(world.bridge.clone());
    assert!(command.availability().is_available());
}

#[tokio::test]
async fn executing_slash_mcp_asks_the_shell_for_the_mcp_panel_exactly_once() {
    let world = agent_world();
    let command = world.commands.get("mcp").unwrap().unwrap();
    assert!(world.bridge.take().is_none());

    command.execute(&world.agent, "").await.unwrap();
    assert_eq!(world.bridge.take(), Some(CapabilityPanel::Mcp));
    assert!(
        world.bridge.take().is_none(),
        "a drained request must not open the panel again"
    );
}

#[tokio::test]
async fn slash_mcp_rejects_invalid_arguments_without_requesting_a_panel() {
    let world = agent_world();
    let command = world.commands.get("mcp").unwrap().unwrap();
    let error = command
        .execute(&world.agent, "list")
        .await
        .expect_err("/mcp list requires the management panel instead");
    assert_eq!(
        error.to_string(),
        "usage: /mcp [enable|disable|reconnect] <server>"
    );
    assert!(world.bridge.take().is_none());
}

#[tokio::test]
async fn slash_mcp_enable_and_disable_mutate_the_shells_exact_stored_owner() {
    let world = agent_world();
    let (management, store) = mcp_fixture();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(management, None);
    state.set_panel_commands(world.bridge.clone());
    let command = world.commands.get("mcp").unwrap().unwrap();
    assert!(command.availability().is_available());

    let seen = Arc::new(Mutex::new(Vec::<heycode_agent::UiEvent>::new()));
    let sink = seen.clone();
    world.agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    });

    command
        .execute(&world.agent, "disable example")
        .await
        .unwrap();
    assert!(!store.load().unwrap()["example"].enabled);
    command
        .execute(&world.agent, "enable example")
        .await
        .unwrap();
    assert!(store.load().unwrap()["example"].enabled);
    assert!(world.bridge.take().is_none());
    let notices = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(notices.iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::Info { text }
            if text.contains("disabled stored MCP definition `example`")
                && text.contains("live connections are unchanged")
    )));
    assert!(notices.iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::Info { text }
            if text.contains("enabled stored MCP definition `example`")
                && text.contains("recomposed")
    )));
}

#[tokio::test]
async fn slash_mcp_refuses_config_owned_mutation_and_does_not_change_the_store() {
    let world = agent_world();
    let (management, store) = mcp_fixture();
    management
        .adopt_configured_servers(BTreeMap::from([(
            "example".to_owned(),
            StoredServer::new("example", McpTransportKind::Stdio, "/configured/example").unwrap(),
        )]))
        .unwrap();
    world.bridge.attach_mcp(management);
    let command = world.commands.get("mcp").unwrap().unwrap();
    let before = store.load().unwrap();

    let error = command
        .execute(&world.agent, "disable example")
        .await
        .expect_err("session configuration remains read-only");
    assert!(
        error
            .to_string()
            .contains("declared in this session's configuration")
    );
    assert_eq!(store.load().unwrap(), before);
    assert!(world.bridge.take().is_none());
}

#[tokio::test]
async fn slash_mcp_reconnect_requires_the_live_runtime_owner_and_never_mutates_state() {
    let world = agent_world();
    let (management, store) = mcp_fixture();
    world.bridge.attach_mcp(management.clone());
    let command = world.commands.get("mcp").unwrap().unwrap();
    let before = store.load().unwrap();

    let error = command
        .execute(&world.agent, "reconnect example")
        .await
        .expect_err("management alone cannot reconnect a process");
    assert_eq!(
        error.to_string(),
        "MCP runtime control is not attached to this shell"
    );
    assert_eq!(store.load().unwrap(), before);
    assert!(world.bridge.take().is_none());

    management.enable("example", false).unwrap();
    let disabled = command
        .execute(&world.agent, "reconnect example")
        .await
        .expect_err("a disabled definition has no live reconnect target");
    assert_eq!(disabled.to_string(), "MCP server `example` is disabled");
    management.enable("example", true).unwrap();

    let unknown = command
        .execute(&world.agent, "reconnect missing")
        .await
        .expect_err("unknown servers remain distinguishable");
    assert_eq!(unknown.to_string(), "no MCP server named `missing`");
    for invalid in [
        "enable",
        "disable bad/name",
        "reconnect example extra",
        "restart example",
    ] {
        assert_eq!(
            command
                .execute(&world.agent, invalid)
                .await
                .expect_err("invalid MCP command")
                .to_string(),
            "usage: /mcp [enable|disable|reconnect] <server>"
        );
    }
    assert_eq!(store.load().unwrap(), before);
}

#[test]
fn the_shell_opens_the_panel_that_owns_the_requested_capability() {
    let bridge = PanelCommandBridge::new();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(mcp_management(), None);
    state.set_plugin_services(plugin_lifecycle(), None);
    state.set_panel_commands(bridge.clone());

    bridge.request(CapabilityPanel::Mcp);
    let panel = state.take_panel_open_request().expect("request pending");
    state.open_capability_panel(panel);
    assert!(state.mcp_panel().is_some(), "/mcp opens the MCP panel");
    assert!(state.plugin_panel().is_none());

    bridge.request(CapabilityPanel::Plugins);
    let panel = state.take_panel_open_request().expect("request pending");
    state.open_capability_panel(panel);
    assert!(
        state.plugin_panel().is_some(),
        "a plugin-panel request opens the plugin panel"
    );
    assert!(state.mcp_panel().is_none());

    // The other order must be just as exclusive: the renderer draws the
    // plugin panel and the key router feeds the MCP panel, so two open
    // panels would send keystrokes to a surface nobody can see.
    bridge.request(CapabilityPanel::Mcp);
    let panel = state.take_panel_open_request().expect("request pending");
    state.open_capability_panel(panel);
    assert!(state.mcp_panel().is_some(), "/mcp opens the MCP panel");
    assert!(
        state.plugin_panel().is_none(),
        "opening the MCP panel must close the plugin panel"
    );
}

fn frame_text(state: &mut AppState) -> String {
    frame_text_at(state, 100, 24)
}

fn frame_text_at(state: &mut AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn skills_catalog_consumes_paste_supports_wheel_and_keeps_a_narrow_selection_visible() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    let skills = (0..20)
        .map(|index| heycode_skills::Skill {
            name: format!("skill-{index:02}"),
            description: format!("Capability row {index:02}"),
            disable_model_invocation: false,
            body: "not rendered".to_owned(),
        })
        .collect::<Vec<_>>();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_capability_services(
        Some(Arc::new(heycode_skills::SkillSet::new(skills).unwrap())),
        None,
        None,
    );
    assert!(state.input.insert_str("preserved draft"));
    state.apply(&heycode_agent::UiEvent::CapabilityPanelRequested {
        panel: heycode_agent::UiPanelId::new("skills").unwrap(),
    });
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("[selected] on skill-00"), "{flat}");

    state.handle_terminal_event(&Event::Paste("/quit\n".to_owned()));
    assert_eq!(state.input.lines(), ["preserved draft"]);
    assert!(
        heycode_tui::ScreenReaderSnapshot::from_state(&state)
            .into_text()
            .contains("type to filter")
    );
    state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("[selected] on skill-01"), "{flat}");

    let rendered = frame_text_at(&mut state, 45, 16);
    assert!(rendered.contains("skill-01"), "{rendered}");
    assert!(flat.contains("enter/space to cycle"), "{flat}");
    state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
    let rendered = frame_text_at(&mut state, 45, 16);
    assert!(rendered.contains("skill-19"), "{rendered}");
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("[selected] on skill-19"), "{flat}");
    state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(!flat.contains("== Skills =="), "{flat}");
    assert_eq!(state.input.lines(), ["preserved draft"]);
}

#[test]
fn a_requested_panel_reaches_the_frame_rather_than_only_the_state() {
    let bridge = PanelCommandBridge::new();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(mcp_management(), None);
    state.set_panel_commands(bridge.clone());
    assert!(!frame_text(&mut state).contains("MCP servers"));

    bridge.request(CapabilityPanel::Mcp);
    let panel = state.take_panel_open_request().expect("request pending");
    state.open_capability_panel(panel);
    let rendered = frame_text(&mut state);
    assert!(rendered.contains("MCP servers"), "{rendered}");
    assert!(rendered.contains("example"), "{rendered}");
}

#[test]
fn a_capability_owner_event_reaches_full_and_flat_renderers() {
    let skills = Arc::new(
        heycode_skills::SkillSet::new(vec![heycode_skills::Skill {
            name: "review".to_owned(),
            description: "Inspect the current change".to_owned(),
            disable_model_invocation: false,
            body: "not rendered".to_owned(),
        }])
        .unwrap(),
    );
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_capability_services(Some(skills), None, None);
    state.apply(&heycode_agent::UiEvent::CapabilityPanelRequested {
        panel: heycode_agent::UiPanelId::new("skills").unwrap(),
    });

    let rendered = frame_text(&mut state);
    assert!(rendered.contains("Skills"), "{rendered}");
    assert!(rendered.contains("review"), "{rendered}");
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("== Skills =="), "{flat}");
    assert!(
        flat.contains("on review; Inspect the current change"),
        "{flat}"
    );
}

#[test]
fn the_shell_opens_nothing_while_no_command_has_asked_for_a_panel() {
    let bridge = PanelCommandBridge::new();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(mcp_management(), None);
    state.set_panel_commands(bridge);
    assert!(state.take_panel_open_request().is_none());
    assert!(state.mcp_panel().is_none());
}

#[test]
fn a_second_request_replaces_the_first_because_one_panel_is_open_at_a_time() {
    let bridge = PanelCommandBridge::new();
    bridge.request(CapabilityPanel::Mcp);
    bridge.request(CapabilityPanel::Plugins);
    assert_eq!(bridge.take(), Some(CapabilityPanel::Plugins));
    assert!(bridge.take().is_none());
}

#[test]
fn attachment_is_recorded_per_panel_rather_than_for_the_shell_as_a_whole() {
    let bridge = PanelCommandBridge::new();
    assert!(!bridge.is_attached(CapabilityPanel::Mcp));
    assert!(!bridge.is_attached(CapabilityPanel::Plugins));
    bridge.attach(CapabilityPanel::Mcp);
    assert!(
        !bridge.is_attached(CapabilityPanel::Mcp),
        "a marker without the MCP owner must not claim availability"
    );
    bridge.attach_mcp(mcp_management());
    assert!(bridge.is_attached(CapabilityPanel::Mcp));
    assert!(
        !bridge.is_attached(CapabilityPanel::Plugins),
        "attaching MCP must not claim the plugin panel is wired"
    );
}

#[tokio::test]
async fn agents_and_hooks_commands_open_their_attached_read_only_catalogs() {
    let world = agent_world();
    let hooks = world
        ._context
        .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
        .unwrap();
    let agents = Arc::new(heycode_agent::SubagentRegistry::new());
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_panel_commands(world.bridge.clone());
    state.set_capability_services(None, Some(agents), Some(hooks));

    for (name, expected) in [
        ("agents", CapabilityPanel::Agents),
        ("hooks", CapabilityPanel::Hooks),
    ] {
        let command = world.commands.get(name).unwrap().unwrap();
        assert!(command.availability().is_available());
        command
            .execute(
                &world.agent,
                if name == "agents" { "providers" } else { "" },
            )
            .await
            .unwrap();
        let requested = state.take_panel_open_request().expect("panel request");
        assert_eq!(requested, expected);
        state.open_capability_panel(requested);
        assert_eq!(
            state.capability_catalog().map(|panel| panel.panel()),
            Some(expected)
        );
        state.close_capability_catalog();
    }
}

#[test]
fn a_skills_owner_event_opens_the_catalog_and_sanitizes_file_metadata() {
    let skills = Arc::new(
        heycode_skills::SkillSet::new(vec![heycode_skills::Skill {
            name: "review".to_owned(),
            description: "Inspect\nchanges\u{1b}[31m".to_owned(),
            disable_model_invocation: true,
            body: "not rendered".to_owned(),
        }])
        .unwrap(),
    );
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_capability_services(Some(skills), None, None);
    state.apply(&heycode_agent::UiEvent::CapabilityPanelRequested {
        panel: heycode_agent::UiPanelId::new("skills").unwrap(),
    });
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("== Skills =="), "{flat}");
    assert!(
        flat.contains("[selected] user-only review; Inspectchanges[31m"),
        "{flat}"
    );
    assert!(
        flat.contains("source: contribution; location: live registry"),
        "{flat}"
    );
    assert!(!flat.contains('\u{1b}'), "{flat:?}");
}

#[tokio::test]
async fn slash_plugins_opens_the_lifecycle_panel_while_verbose_keeps_inventory_output() {
    let world = agent_world();
    let seen = Arc::new(Mutex::new(Vec::<heycode_agent::UiEvent>::new()));
    let sink = seen.clone();
    world.agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    });
    let command = world.commands.get("plugins").unwrap().unwrap();
    command.execute(&world.agent, "").await.unwrap();
    assert!(
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|event| matches!(
                event,
                heycode_agent::UiEvent::CapabilityPanelRequested { panel }
                    if panel.as_str() == "plugins"
            ))
    );

    command.execute(&world.agent, "verbose").await.unwrap();
    assert!(seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|event| matches!(event, heycode_agent::UiEvent::Info { text } if text.contains("plugins:"))));
}

#[test]
fn every_capability_panel_names_itself_and_its_detached_reason_distinctly() {
    let ids: Vec<&str> = CapabilityPanel::ALL
        .into_iter()
        .map(CapabilityPanel::as_str)
        .collect();
    assert_eq!(
        ids,
        [
            "mcp",
            "plugins",
            "skills",
            "agents",
            "hooks",
            "settings",
            "workflows"
        ]
    );
    let reasons: Vec<&str> = CapabilityPanel::ALL
        .into_iter()
        .map(CapabilityPanel::detached_reason)
        .collect();
    assert_eq!(
        reasons,
        [
            "MCP management is not attached to this shell",
            "Plugin lifecycle is not attached to this shell",
            "Skills are not attached to this shell",
            "Subagent registry is not attached to this shell",
            "Hook registry is not attached to this shell",
            "Settings are not attached to this shell",
            "Workflow service is not attached to this shell"
        ]
    );
}

struct ReadinessFixture {
    descriptor: heycode_agent::SubagentProviderDescriptor,
    outcome: Option<heycode_agent::subagent_provider::SubagentReadiness>,
    entered: Arc<std::sync::atomic::AtomicUsize>,
    tokens: Arc<Mutex<Vec<tokio_util::sync::CancellationToken>>>,
}

#[async_trait::async_trait]
impl heycode_agent::SubagentProvider for ReadinessFixture {
    fn descriptor(&self) -> &heycode_agent::SubagentProviderDescriptor {
        &self.descriptor
    }
    async fn readiness(
        &self,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::subagent_provider::SubagentReadiness, heycode_agent::SubagentError>
    {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.tokens.lock().unwrap().push(cancellation.clone());
        if let Some(outcome) = self.outcome {
            return Ok(outcome);
        }
        cancellation.cancelled().await;
        Err(heycode_agent::SubagentError::new(
            heycode_agent::SubagentErrorCode::Cancelled,
            "cancelled fixture",
        ))
    }
    async fn start(
        &self,
        _: heycode_agent::SubagentRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::SubagentStarted, heycode_agent::SubagentError> {
        panic!("catalog must never start or authenticate a child")
    }
}

struct NoReadinessProbe(heycode_agent::SubagentProviderDescriptor);
#[async_trait::async_trait]
impl heycode_agent::SubagentProvider for NoReadinessProbe {
    fn descriptor(&self) -> &heycode_agent::SubagentProviderDescriptor {
        &self.0
    }
    async fn start(
        &self,
        _: heycode_agent::SubagentRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::SubagentStarted, heycode_agent::SubagentError> {
        panic!("catalog must never start a child")
    }
}

fn readiness_descriptor(id: &str) -> heycode_agent::SubagentProviderDescriptor {
    heycode_agent::SubagentProviderDescriptor::new(
        id,
        id,
        heycode_agent::SubagentCapabilities {
            fork: heycode_llm::CapabilitySupport::Unsupported,
            continuation: heycode_llm::CapabilitySupport::Unknown,
            interrupt: heycode_llm::CapabilitySupport::Supported,
        },
    )
    .unwrap()
}

async fn wait_for_readiness(state: &mut AppState, expected: &str) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            state.poll_agent_readiness();
            let output = heycode_tui::ScreenReaderSnapshot::from_state(state).into_text();
            if output.contains(expected) {
                return output;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn agents_readiness_command_emits_missing_auth_unknown_ready_and_capability_evidence() {
    use heycode_agent::subagent_provider::SubagentReadiness;
    let world = agent_world();
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    for (id, outcome) in [
        ("missing-credential", SubagentReadiness::NeedsAuthentication),
        ("supported-runtime", SubagentReadiness::Ready),
        ("missing-runtime", SubagentReadiness::Unavailable),
    ] {
        registry
            .register(Arc::new(ReadinessFixture {
                descriptor: readiness_descriptor(id),
                outcome: Some(outcome),
                entered: Arc::default(),
                tokens: Arc::default(),
            }))
            .unwrap();
    }
    registry
        .register(Arc::new(NoReadinessProbe(readiness_descriptor(
            "unsupported-probe",
        ))))
        .unwrap();
    let mut state = AppState::new("model", "/workspace".into());
    state.set_panel_commands(world.bridge.clone());
    state.set_capability_services(None, Some(registry), None);
    world
        .commands
        .get("agents")
        .unwrap()
        .unwrap()
        .execute(&world.agent, "providers")
        .await
        .unwrap();
    let requested = state.take_panel_open_request().unwrap();
    state.open_capability_panel(requested);
    let output = wait_for_readiness(&mut state, "Unknown (provider cannot prove readiness)").await;
    for expected in [
        "NeedsAuthentication",
        "Ready",
        "Unavailable",
        "fork=unsupported continuation=unknown interrupt=supported",
        "R rechecks readiness",
        "C cancels probes",
    ] {
        assert!(output.contains(expected), "{output}");
    }
    while state.capability_catalog().unwrap().rows()[state.capability_catalog().unwrap().selected()]
        .name()
        != "missing-credential"
    {
        state.handle_terminal_event(&crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
        ));
    }
    let frame = frame_text(&mut state);
    assert!(frame.contains("NeedsAuthentication"), "{frame}");
    assert!(frame.contains("fork=unsupported"), "{frame}");
    assert!(frame.contains("provider's setup"), "{frame}");
}

#[tokio::test]
async fn agents_readiness_cancel_is_visible_bounded_and_cannot_repopulate_a_closed_panel() {
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tokens = Arc::new(Mutex::new(Vec::new()));
    for index in 0..40 {
        registry
            .register(Arc::new(ReadinessFixture {
                descriptor: readiness_descriptor(&format!("held-{index:02}")),
                outcome: None,
                entered: entered.clone(),
                tokens: tokens.clone(),
            }))
            .unwrap();
    }
    let mut state = AppState::new("model", "/workspace".into());
    state.set_capability_services(None, Some(registry), None);
    state.open_capability_panel(CapabilityPanel::Agents);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "at most four provider probes may run concurrently"
    );
    let before = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(before.contains("Unknown (probe limit)"), "{before}");
    state.handle_terminal_event(&crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::NONE,
        ),
    ));
    let output = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(output.contains("Unknown (cancelled)"), "{output}");
    assert!(
        tokens
            .lock()
            .unwrap()
            .iter()
            .all(tokio_util::sync::CancellationToken::is_cancelled)
    );
    assert!(frame_text(&mut state).contains("cancelled"));
    state.close_capability_catalog();
    state.poll_agent_readiness();
    assert!(state.capability_catalog().is_none());
    state.open_capability_panel(CapabilityPanel::Agents);
    let reopened = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(reopened.contains("Unknown (checking)"), "{reopened}");
    assert!(
        !reopened.contains("cancelled"),
        "stale cancellation must not poison a new panel"
    );
    state.close_capability_catalog();
    assert!(
        tokens
            .lock()
            .unwrap()
            .iter()
            .all(tokio_util::sync::CancellationToken::is_cancelled)
    );
}

#[tokio::test]
async fn agents_readiness_timeout_is_unknown_in_emitted_output() {
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    let tokens = Arc::new(Mutex::new(Vec::new()));
    registry
        .register(Arc::new(ReadinessFixture {
            descriptor: readiness_descriptor("slow-runtime"),
            outcome: None,
            entered: Arc::default(),
            tokens: tokens.clone(),
        }))
        .unwrap();
    let mut state = AppState::new("model", "/workspace".into());
    state.set_capability_services(None, Some(registry), None);
    state.open_capability_panel(CapabilityPanel::Agents);
    let output = wait_for_readiness(&mut state, "Unknown (timed out)").await;
    assert!(
        !output.contains("NeedsAuthentication"),
        "timeout is not evidence of missing credentials"
    );
    assert!(frame_text(&mut state).contains("Unknown (timed out)"));
    assert!(
        tokens
            .lock()
            .unwrap()
            .iter()
            .all(tokio_util::sync::CancellationToken::is_cancelled)
    );
}

#[tokio::test]
async fn agents_providers_keeps_catalog_explicit_and_rejects_unknown_views() {
    let world = agent_world();
    let command = world.commands.get("agents").unwrap().unwrap();
    assert_eq!(command.descriptor().synopsis(), "/agents [view]");
    command.execute(&world.agent, "providers").await.unwrap();
    assert_eq!(world.bridge.take(), Some(CapabilityPanel::Agents));
    assert!(world.bridge.take().is_none());
    let error = command
        .execute(&world.agent, "missing-view")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("/agents [providers]"));
    assert!(world.bridge.take().is_none());
}

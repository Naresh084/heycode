//! Q03 deterministic trust/setup/commands/MCP/provider journey snapshots.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandRegistry,
    CommandSource, CommandTiming,
};
use heycode_mcp::management::{
    McpDefinitionStore, McpHealth, McpManagement, McpManagementError, McpProbe, StoredServer,
};
use heycode_tui::app::AppState;
use heycode_tui::route_picker::{RoutePickerClass, RoutePickerRow, RoutePickerSelection};

use crate::support::journey::{JourneyHarness, key, key_with};

fn base_state() -> AppState {
    let mut state = AppState::new("model-a", std::path::PathBuf::from("/workspace"));
    state.runtime = "native".to_owned();
    state.provider = "provider-a".to_owned();
    state.permission = "ask".to_owned();
    state
}

#[test]
fn trust_journey_replays_typed_state_and_keyboard_focus_without_terminal_timing() {
    use heycode_trust::{ProjectContentPolicy, UntrustedProjectAccess, WorkspaceTrustService};

    let root = tempfile::tempdir().unwrap();
    let service = WorkspaceTrustService::memory(
        root.path(),
        ProjectContentPolicy::new(UntrustedProjectAccess::Block, UntrustedProjectAccess::Block),
    )
    .unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let prompt = service.dialog_prompt().unwrap();
    let mut journey = JourneyHarness::new(AppState::new("model-a", canonical.clone()));
    journey.normalize(canonical.display().to_string(), "<WORKSPACE>");
    journey.action("open typed workspace trust prompt", |state| {
        state.receive_workspace_trust(prompt);
    });
    journey.terminal(
        "move focus from restricted to persistent trust",
        key(crossterm::event::KeyCode::Up),
    );

    assert_eq!(journey.frames()[0].kind, "action");
    assert_eq!(journey.frames()[1].kind, "terminal");
    assert_eq!(
        journey.frames()[0].frame,
        "== Workspace trust ==\nTrust this workspace?\nworkspace: <WORKSPACE>\nproject instructions: blocked\nproject settings: blocked\nproject plugins, MCP, and hooks: blocked\n[ ] Trust once — Enable project executable contributions for this process.\n[ ] Trust this workspace — Save trust for this canonical workspace identity.\n[selected] Open read-only — Keep project executable contributions disabled.\n[ ] Exit — Leave without changing workspace trust.\nkeys: Up or Down chooses; Enter confirms; Escape exits; Control+C twice exits."
    );
    assert_eq!(
        journey.frames()[1].frame,
        "== Workspace trust ==\nTrust this workspace?\nworkspace: <WORKSPACE>\nproject instructions: blocked\nproject settings: blocked\nproject plugins, MCP, and hooks: blocked\n[ ] Trust once — Enable project executable contributions for this process.\n[selected] Trust this workspace — Save trust for this canonical workspace identity.\n[ ] Open read-only — Keep project executable contributions disabled.\n[ ] Exit — Leave without changing workspace trust.\nkeys: Up or Down chooses; Enter confirms; Escape exits; Control+C twice exits."
    );
}

#[test]
fn setup_journey_replays_the_plugin_owned_wizard_and_real_key_router() {
    let onboarding = Arc::new(heycode_onboarding::OnboardingService::new(true));
    let mut journey = JourneyHarness::new(base_state());
    journey.action("attach first-run onboarding service", |state| {
        state.set_onboarding(onboarding);
    });
    journey.terminal("choose subscription", key(crossterm::event::KeyCode::Enter));
    let welcome = "== Setup ==\nWelcome to heycode\nChoose how you'd like to connect.\n[selected] Use a subscription — Connect your ChatGPT, Claude or Grok account.\n[ ] Use a local model — Connect to LM Studio, Ollama or a custom server.\n[ ] Select a provider — Connect to your API provider.\nkeys: Up or Down chooses; Tab moves forward; Enter confirms; Escape cancels; Control+C twice exits.";
    assert_eq!(journey.frames()[0].frame, welcome);
    assert_eq!(journey.frames()[1].frame, welcome);
}

struct FixtureCommand {
    descriptor: CommandDescriptor,
}

#[async_trait]
impl Command for FixtureCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        CommandAvailability::available()
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, _args: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn commands() -> Arc<CommandRegistry> {
    let mut registry = CommandRegistry::new();
    let source = CommandSource::from_plugin("commands").unwrap();
    for descriptor in [
        CommandDescriptor::new(
            "model",
            "Switch the active model",
            vec![CommandArgument::optional("id", "Model id").unwrap()],
            CommandTiming::Queued,
            source.clone(),
        )
        .unwrap(),
        CommandDescriptor::new(
            "status",
            "Show effective state",
            Vec::new(),
            CommandTiming::Immediate,
            source,
        )
        .unwrap(),
    ] {
        registry
            .register(Arc::new(FixtureCommand { descriptor }))
            .unwrap();
    }
    Arc::new(registry)
}

#[test]
fn commands_journey_replays_live_catalog_filtering_as_stable_frames() {
    let mut state = base_state();
    state.set_commands(commands());
    let mut journey = JourneyHarness::new(state);
    journey.terminal(
        "open command palette with Control+P",
        key_with(
            crossterm::event::KeyCode::Char('p'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    );
    journey.terminal(
        "paste exact status query",
        crossterm::event::Event::Paste("status".to_owned()),
    );

    assert_eq!(
        journey.frames()[0].frame,
        concat!(
            "== heycode ==\nversion: 0.1.0\nroute: provider-a/model-a\nworkspace: /workspace\n",
            "== Transcript ==\nempty\n",
            "== Status ==\napproval policy: ask\ncontext: unavailable\ntokens: in 0 out 0\nactivity: ready\n",
            "== Composer ==\ninput: /\nattachments: 0\nkeys: Enter sends; Alt+Enter or backslash then Enter inserts a new line; Control+P opens commands; Escape interrupts; Page Up and Page Down scroll.\n",
            "== Command palette ==\nquery: empty\n[selected] /model [id] — Switch the active model; source: commands; timing: queued; available.\n[ ] /status — Show effective state; source: commands; timing: immediate; available.\nkeys: Up or Down chooses; Enter runs, or completes a command that takes arguments; Escape closes."
        )
    );
    assert_eq!(
        journey.frames()[1].frame,
        concat!(
            "== heycode ==\nversion: 0.1.0\nroute: provider-a/model-a\nworkspace: /workspace\n",
            "== Transcript ==\nempty\n",
            "== Status ==\napproval policy: ask\ncontext: unavailable\ntokens: in 0 out 0\nactivity: ready\n",
            "== Composer ==\ninput: /status\nattachments: 0\nkeys: Enter sends; Alt+Enter or backslash then Enter inserts a new line; Control+P opens commands; Escape interrupts; Page Up and Page Down scroll.\n",
            "== Command palette ==\nquery: status\n[selected] /status — Show effective state; source: commands; timing: immediate; available.\nkeys: Up or Down chooses; Enter runs, or completes a command that takes arguments; Escape closes."
        )
    );
}

#[derive(Default)]
struct TestStore {
    rows: Mutex<BTreeMap<String, StoredServer>>,
}

impl McpDefinitionStore for TestStore {
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        Ok(self.rows.lock().unwrap().clone())
    }

    fn persist(&self, rows: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> {
        *self.rows.lock().unwrap() = rows.clone();
        Ok(())
    }
}

struct Reachable;

impl McpProbe for Reachable {
    fn probe(&self, _server: &StoredServer) -> McpHealth {
        McpHealth::Reachable
    }
}

#[test]
fn mcp_journey_replays_the_shared_management_panel_without_a_server_or_network() {
    let server = StoredServer::new(
        "docs",
        heycode_mcp::McpTransportKind::Stdio,
        "/usr/bin/docs-mcp --serve",
    )
    .unwrap();
    let store = Arc::new(TestStore::default());
    store
        .rows
        .lock()
        .unwrap()
        .insert(server.name.clone(), server);
    let management = Arc::new(McpManagement::new(store).with_probe(Arc::new(Reachable)));
    let mut state = base_state();
    state.set_mcp_services(management, None);
    let mut journey = JourneyHarness::new(state);
    journey.action("open MCP panel from shared management state", |state| {
        state.open_mcp_panel();
    });
    journey.terminal(
        "move from Status to Auth section",
        key(crossterm::event::KeyCode::Tab),
    );
    journey.action("refresh and probe every server", |state| {
        state.refresh_mcp_panel_probed();
    });

    let base = concat!(
        "== heycode ==\nversion: 0.1.0\nroute: provider-a/model-a\nworkspace: /workspace\n",
        "== Transcript ==\nempty\n",
        "== Status ==\napproval policy: ask\ncontext: unavailable\ntokens: in 0 out 0\nactivity: ready\n",
        "== Composer ==\ninput: empty\nattachments: 0\nkeys: Enter sends; Alt+Enter or backslash then Enter inserts a new line; Control+P opens commands; Escape interrupts; Page Up and Page Down scroll.\n"
    );
    assert_eq!(
        journey.frames()[0].frame,
        format!(
            "{base}== MCP ==\nservers (1)\n● docs stdio enabled unknown\n[status] auth tools resources prompts actions\nenabled · user scope · stdio\ntarget: /usr/bin/docs-mcp --serve\nconnection: no live registry evidence in this world\nkeys: Up or Down moves between servers; Tab changes section; Enter opens actions; Escape closes."
        ),
        "opening the panel reads definitions; it does not start the servers"
    );
    assert_eq!(
        journey.frames()[1].frame,
        format!(
            "{base}== MCP ==\nservers (1)\n● docs stdio enabled unknown\nstatus [auth] tools resources prompts actions\nprobe: unknown\nnot probed yet; run test, or refresh and probe, to check\nkeys: Up or Down moves between servers; Tab changes section; Enter opens actions; Escape closes."
        )
    );
    assert_eq!(
        journey.frames()[2].frame,
        format!(
            "{base}== MCP ==\nservers (1)\n● docs stdio enabled reachable\nstatus [auth] tools resources prompts actions\nprobe: reachable\nthe server answered initialize\nrefreshed\nkeys: Up or Down moves between servers; Tab changes section; Enter opens actions; Escape closes."
        ),
        "an explicit refresh is what connects"
    );
}

fn route_rows() -> Vec<RoutePickerRow> {
    vec![
        RoutePickerRow {
            id: "native".to_owned(),
            display_name: "heycode native".to_owned(),
            class: RoutePickerClass::NativeAgent,
            detail: "resume · compact".to_owned(),
            current: true,
            selection: Some(RoutePickerSelection::NativeRuntime {
                runtime: "native".to_owned(),
            }),
            unavailable_reason: None,
        },
        RoutePickerRow {
            id: "provider-a".to_owned(),
            display_name: "Provider A".to_owned(),
            class: RoutePickerClass::InferenceApi,
            detail: "default model model-a".to_owned(),
            current: true,
            selection: Some(RoutePickerSelection::Inference {
                provider: "provider-a".to_owned(),
                default_model: "model-a".to_owned(),
            }),
            unavailable_reason: None,
        },
        RoutePickerRow {
            id: "delegated".to_owned(),
            display_name: "Delegated agent".to_owned(),
            class: RoutePickerClass::DelegatedAgent,
            detail: "resume unproven".to_owned(),
            current: false,
            selection: None,
            unavailable_reason: Some("primary runtime bridge incomplete".to_owned()),
        },
    ]
}

#[test]
fn provider_journey_replays_registry_rows_filters_and_keyboard_focus() {
    let mut journey = JourneyHarness::new(base_state());
    journey.action("open and populate provider/runtime picker", |state| {
        state.open_route_picker("provider-a", "native");
        state.apply_route_catalog(route_rows());
    });
    journey.terminal(
        "filter to inference APIs with Tab",
        key(crossterm::event::KeyCode::Tab),
    );

    let base = concat!(
        "== heycode ==\nversion: 0.1.0\nroute: provider-a/model-a\nworkspace: /workspace\n",
        "== Transcript ==\nempty\n",
        "== Status ==\napproval policy: ask\ncontext: unavailable\ntokens: in 0 out 0\nactivity: ready\n",
        "== Composer ==\ninput: empty\nattachments: 0\nkeys: Enter sends; Alt+Enter or backslash then Enter inserts a new line; Control+P opens commands; Escape interrupts; Page Up and Page Down scroll.\n"
    );
    assert_eq!(
        journey.frames()[0].frame,
        format!(
            "{base}== Provider and runtime ==\nfilter: all\nquery: empty\n[selected] NATIVE AGENT native — heycode native — resume · compact — current — available.\n[ ] INFERENCE API provider-a — Provider A — default model model-a — current — available.\n[ ] DELEGATED AGENT delegated — Delegated agent — resume unproven — unavailable: primary runtime bridge incomplete.\nkeys: Tab changes filter; Up or Down chooses; Enter selects; Escape closes."
        )
    );
    assert_eq!(
        journey.frames()[1].frame,
        format!(
            "{base}== Provider and runtime ==\nfilter: inference\nquery: empty\n[selected] INFERENCE API provider-a — Provider A — default model model-a — current — available.\nkeys: Tab changes filter; Up or Down chooses; Enter selects; Escape closes."
        )
    );

    assert_eq!(
        journey.transcript().matches("== step").count(),
        2,
        "each explicit stimulus produces exactly one stable frame"
    );
}

#[test]
fn mcp_without_a_configured_server_answers_with_a_receipt_rather_than_an_empty_panel() {
    let management = Arc::new(McpManagement::new(Arc::new(TestStore::default())));
    let mut state = base_state();
    state.set_mcp_services(management, None);
    state.open_mcp_panel();
    assert!(
        state.mcp_panel().is_none(),
        "an empty panel teaches nothing about how to configure a server"
    );
    let snapshot = heycode_tui::ScreenReaderSnapshot::from_state(&state);
    let rendered = snapshot.as_text();
    assert!(
        rendered.contains("No MCP servers configured."),
        "{rendered}"
    );
    assert!(rendered.contains("heycode mcp add"), "{rendered}");
    assert!(rendered.contains("[mcp.servers]"), "{rendered}");
}

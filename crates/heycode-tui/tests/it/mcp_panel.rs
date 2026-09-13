//! U12 MCP panel projection, honesty, action and disposal contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use heycode_core::{Context, Plugin, compose};
use heycode_mcp::management::{
    McpDefinitionStore, McpHealth, McpManagement, McpManagementError, McpOperation, McpProbe,
    McpStatusRow, StoredServer,
};
use heycode_mcp::{
    McpCapabilitySet, McpConnectionProviderId, McpContributionCounts, McpDefinitionScope,
    McpGenerationCandidate, McpRegistry, McpServerDefinition, McpServerId, McpSnapshot,
    McpStdioTransport, McpTransportDefinition, McpTransportKind,
};
use heycode_tui::app::AppState;
use heycode_tui::mcp_panel::{
    McpListingSupport, McpPanelIntent, McpPanelKeyOutcome, McpPanelOutcome, McpPanelRow,
    McpPanelSection, McpPanelTone, McpPanelView, McpSectionState, build_actions, build_rows,
    dispatch, display_target, health_next_step, health_word, panel_descriptor, section_view,
};
use heycode_ui::{SERVICE_UI, UiContributionId, UiRegistry, UiSlot, ui_registry_plugin};
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::TestBackend};

const URL_SECRET: &str = "SUPERSECRETTOKEN";

#[derive(Default)]
struct TestStore {
    servers: Mutex<BTreeMap<String, StoredServer>>,
}

impl TestStore {
    fn with(servers: Vec<StoredServer>) -> Arc<Self> {
        Arc::new(Self {
            servers: Mutex::new(
                servers
                    .into_iter()
                    .map(|server| (server.name.clone(), server))
                    .collect(),
            ),
        })
    }

    fn names(&self) -> Vec<String> {
        self.servers.lock().unwrap().keys().cloned().collect()
    }

    fn get(&self, name: &str) -> Option<StoredServer> {
        self.servers.lock().unwrap().get(name).cloned()
    }
}

impl McpDefinitionStore for TestStore {
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        Ok(self.servers.lock().unwrap().clone())
    }

    fn persist(&self, servers: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> {
        *self.servers.lock().unwrap() = servers.clone();
        Ok(())
    }
}

struct FixedProbe(McpHealth);

impl McpProbe for FixedProbe {
    fn probe(&self, _server: &StoredServer) -> McpHealth {
        self.0
    }
}

fn stdio(name: &str) -> StoredServer {
    StoredServer::new(
        name,
        McpTransportKind::Stdio,
        "/usr/bin/example-mcp --serve",
    )
    .unwrap()
}

fn managed(name: &str) -> StoredServer {
    let mut server = stdio(name);
    server.scope = McpDefinitionScope::Managed;
    server.managed = true;
    server
}

fn status(server: StoredServer, health: McpHealth) -> McpStatusRow {
    McpStatusRow { server, health }
}

fn row(server: StoredServer, health: McpHealth) -> McpPanelRow {
    build_rows(&[status(server, health)], None)
        .pop()
        .expect("one projected row")
}

/// A registry snapshot for `id` with one committed generation.
fn live_snapshot(
    id: &str,
    capabilities: McpCapabilitySet,
    contributions: McpContributionCounts,
) -> (Context, Arc<McpSnapshot>) {
    let registry = McpRegistry::new();
    let owner = Context::new();
    let transport =
        McpStdioTransport::new("/usr/bin/example-mcp", "/tmp", Vec::new(), BTreeMap::new())
            .unwrap();
    registry
        .register_definition(
            &owner,
            McpServerDefinition::new(
                id,
                "Example",
                McpDefinitionScope::User,
                McpTransportDefinition::Stdio(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &owner,
            &McpServerId::new(id).unwrap(),
            McpConnectionProviderId::new("stdio-local").unwrap(),
            10,
        )
        .unwrap();
    publisher
        .publish_generation(
            McpGenerationCandidate::new(
                "2025-11-25",
                "example-server",
                "4.2.0",
                capabilities,
                contributions,
                20,
            )
            .unwrap(),
        )
        .unwrap();
    let snapshot = registry.snapshot().unwrap();
    (owner, snapshot)
}

fn advertising_everything() -> McpCapabilitySet {
    McpCapabilitySet {
        tools: true,
        resources: true,
        prompts: true,
        ..McpCapabilitySet::default()
    }
}

fn management(store: &Arc<TestStore>) -> McpManagement {
    McpManagement::new(store.clone())
}

fn press(panel: &mut McpPanelView, code: KeyCode) -> McpPanelKeyOutcome {
    panel.handle_key(code, KeyModifiers::NONE)
}

/// Move the cursor onto `operation` in the actions section.
///
/// Both loops are bounded by the closed set they walk: an unreachable section
/// or action must fail a named test, never spin a test binary forever.
fn open_actions(panel: &mut McpPanelView, operation: McpOperation) {
    for _ in 0..McpPanelSection::ALL.len() {
        if panel.section() == McpPanelSection::Actions {
            break;
        }
        press(panel, KeyCode::Tab);
    }
    assert_eq!(
        panel.section(),
        McpPanelSection::Actions,
        "tab order must reach the actions section"
    );
    let index = McpOperation::ALL
        .iter()
        .position(|candidate| *candidate == operation)
        .expect("operation is in the closed set");
    for _ in 0..McpOperation::ALL.len() {
        if panel.action_selected() == index {
            break;
        }
        press(panel, KeyCode::Down);
    }
    assert_eq!(
        panel.action_selected(),
        index,
        "the action cursor must reach {operation}"
    );
}

fn key(code: KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn render_rows(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

fn rendered_control(rows: &[String], needle: &str) -> (u16, u16) {
    rows.iter()
        .enumerate()
        .find_map(|(row, text)| {
            text.find(needle)
                .map(|column| (u16::try_from(column).unwrap(), u16::try_from(row).unwrap()))
        })
        .unwrap_or_else(|| panic!("`{needle}` was not rendered in {rows:#?}"))
}

#[test]
fn panel_offers_every_mcp10_operation_exactly_once_in_the_closed_order() {
    let selected = row(stdio("github"), McpHealth::Reachable);
    let offered = build_actions(Some(&selected))
        .into_iter()
        .map(|action| action.operation)
        .collect::<Vec<_>>();
    assert_eq!(offered, McpOperation::ALL.to_vec());
}

#[test]
fn a_managed_server_shows_its_lock_and_is_never_offered_a_mutating_action() {
    let locked = row(managed("corp"), McpHealth::Reachable);
    let actions = build_actions(Some(&locked));
    let refused = actions
        .iter()
        .filter(|action| action.unavailable_reason.is_some())
        .map(|action| action.operation)
        .collect::<Vec<_>>();
    assert_eq!(
        refused,
        vec![
            McpOperation::Edit,
            McpOperation::Enable,
            McpOperation::Remove
        ]
    );
    for action in &actions {
        if let Some(reason) = action.unavailable_reason.as_deref() {
            assert!(reason.contains("managed by an administrator"), "{reason}");
            assert!(reason.contains("corp"), "{reason}");
        }
    }
    let status = section_view(
        McpPanelSection::Status,
        Some(&locked),
        McpListingSupport::CURRENT,
    );
    assert!(
        status
            .lines
            .iter()
            .any(|line| line.contains("managed by an administrator")),
        "{status:?}"
    );
}

#[test]
fn an_unrecognized_health_state_reads_as_unknown_and_never_as_reachable() {
    // `McpHealth` is `#[non_exhaustive]`, so a state from a newer build cannot
    // be constructed here — but it would take the same wildcard arm `Unknown`
    // takes, which is what this pins.
    assert_eq!(health_word(McpHealth::Unknown), "unknown");
    for health in [
        McpHealth::Unknown,
        McpHealth::Unreachable,
        McpHealth::AuthorizationRequired,
    ] {
        assert_ne!(health_word(health), "reachable", "{health:?}");
    }
    assert_eq!(health_word(McpHealth::Reachable), "reachable");
}

#[test]
fn needs_auth_and_unreachable_send_the_operator_to_different_next_steps() {
    assert_ne!(
        health_word(McpHealth::AuthorizationRequired),
        health_word(McpHealth::Unreachable)
    );
    assert_ne!(
        health_next_step(McpHealth::AuthorizationRequired),
        health_next_step(McpHealth::Unreachable)
    );
    let needs_auth = row(stdio("auth"), McpHealth::AuthorizationRequired);
    let unreachable = row(stdio("down"), McpHealth::Unreachable);
    let authorize = section_view(
        McpPanelSection::Auth,
        Some(&needs_auth),
        McpListingSupport::CURRENT,
    );
    let broken = section_view(
        McpPanelSection::Auth,
        Some(&unreachable),
        McpListingSupport::CURRENT,
    );
    assert!(
        authorize.lines.iter().any(|line| line.contains("run auth")),
        "{authorize:?}"
    );
    assert!(
        broken
            .lines
            .iter()
            .any(|line| line.contains("command or URL")),
        "{broken:?}"
    );
    assert_ne!(authorize.lines, broken.lines);
}

/// A walked zero and an unasked zero must not render the same.
///
/// This test used to assert the opposite arm: before MCP08/MCP09 both families
/// reported themselves `Unsupported`, because a `0` would have claimed the
/// server had none when heycode had never asked. Both listings are live now, so
/// the honest distinction moved rather than disappeared — a server that
/// advertises a family and walked it to zero renders a truthful `0`, and a
/// server that never advertised it renders `NotAdvertised`, still never `0`.
#[test]
fn resources_and_prompts_separate_a_walked_zero_from_a_never_advertised_one() {
    let (_walked_owner, walked) = live_snapshot(
        "example",
        advertising_everything(),
        McpContributionCounts {
            tools: 3,
            resources: 0,
            prompts: 0,
        },
    );
    let walked_rows = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(walked.as_ref()),
    );
    let walked_row = walked_rows.first().expect("one row");

    let (_silent_owner, silent) = live_snapshot(
        "example",
        McpCapabilitySet {
            tools: true,
            ..McpCapabilitySet::default()
        },
        McpContributionCounts {
            tools: 3,
            resources: 0,
            prompts: 0,
        },
    );
    let silent_rows = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(silent.as_ref()),
    );
    let silent_row = silent_rows.first().expect("one row");

    for section in [McpPanelSection::Resources, McpPanelSection::Prompts] {
        let noun = section.title();

        let walked_view = section_view(section, Some(walked_row), McpListingSupport::CURRENT);
        assert_eq!(walked_view.state, McpSectionState::Live, "{walked_view:?}");
        assert!(
            walked_view
                .lines
                .iter()
                .any(|line| line.contains(&format!("0 {noun}"))),
            "a family heycode walked to zero states the zero: {walked_view:?}"
        );

        let silent_view = section_view(section, Some(silent_row), McpListingSupport::CURRENT);
        assert_eq!(
            silent_view.state,
            McpSectionState::NotAdvertised,
            "{silent_view:?}"
        );
        assert!(
            !silent_view
                .lines
                .iter()
                .any(|line| line.contains(&format!("0 {noun}"))
                    || line.contains("in the current generation")),
            "a family the server never advertised must not render a count: {silent_view:?}"
        );
        assert_ne!(walked_view.lines, silent_view.lines);
    }
}

/// The `Unsupported` arm stays reachable for the next family that lands.
///
/// Every field of [`McpListingSupport::CURRENT`] is `true` today, so nothing in
/// the product reaches this arm. It is still the arm a future listing family
/// sits in between "the section exists" and "the code lists it", and this test
/// is what keeps it working through that gap rather than rotting unexercised.
#[test]
fn an_unimplemented_listing_names_its_row_instead_of_rendering_a_count() {
    let (_owner, snapshot) = live_snapshot(
        "example",
        advertising_everything(),
        McpContributionCounts {
            tools: 3,
            resources: 0,
            prompts: 0,
        },
    );
    let projected = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(snapshot.as_ref()),
    );
    let selected = projected.first().expect("one row");
    let unlanded = McpListingSupport {
        resources: false,
        prompts: false,
        ..McpListingSupport::CURRENT
    };
    for (section, owning_row) in [
        (McpPanelSection::Resources, "MCP08"),
        (McpPanelSection::Prompts, "MCP09"),
    ] {
        let view = section_view(section, Some(selected), unlanded);
        assert_eq!(view.state, McpSectionState::Unsupported, "{view:?}");
        assert!(
            view.lines.iter().any(|line| line.contains(owning_row)),
            "{view:?}"
        );
        assert!(
            !view
                .lines
                .iter()
                .any(|line| line.contains(&format!("0 {}", section.title()))
                    || line.contains("in the current generation")),
            "an unimplemented listing must not render a count: {view:?}"
        );
    }
}

#[test]
fn a_walked_listing_renders_the_count_it_actually_found() {
    let (_owner, snapshot) = live_snapshot(
        "example",
        advertising_everything(),
        McpContributionCounts {
            tools: 1,
            resources: 4,
            prompts: 7,
        },
    );
    let projected = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(snapshot.as_ref()),
    );
    let selected = projected.first().expect("one row");
    for (section, expected) in [
        (McpPanelSection::Resources, '4'),
        (McpPanelSection::Prompts, '7'),
    ] {
        let view = section_view(section, Some(selected), McpListingSupport::CURRENT);
        assert_eq!(view.state, McpSectionState::Live, "{view:?}");
        assert!(
            view.lines.iter().any(|line| line.contains(expected)),
            "{view:?}"
        );
    }
}

#[test]
fn tools_separate_no_connection_from_not_advertised_from_a_counted_generation() {
    let offline = row(stdio("example"), McpHealth::Unknown);
    let offline_view = section_view(
        McpPanelSection::Tools,
        Some(&offline),
        McpListingSupport::CURRENT,
    );
    assert_eq!(offline_view.state, McpSectionState::NoEvidence);

    let (_silent_owner, silent) = live_snapshot(
        "example",
        McpCapabilitySet::default(),
        McpContributionCounts::default(),
    );
    let silent_row = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(silent.as_ref()),
    );
    assert_eq!(
        section_view(
            McpPanelSection::Tools,
            silent_row.first(),
            McpListingSupport::CURRENT,
        )
        .state,
        McpSectionState::NotAdvertised
    );

    let (_live_owner, live) = live_snapshot(
        "example",
        advertising_everything(),
        McpContributionCounts {
            tools: 7,
            resources: 0,
            prompts: 0,
        },
    );
    let live_row = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(live.as_ref()),
    );
    let view = section_view(
        McpPanelSection::Tools,
        live_row.first(),
        McpListingSupport::CURRENT,
    );
    assert_eq!(view.state, McpSectionState::Live);
    assert!(view.lines.iter().any(|line| line.contains('7')), "{view:?}");
}

#[test]
fn status_states_the_absence_of_live_evidence_rather_than_implying_an_idle_server() {
    let offline = row(stdio("example"), McpHealth::Unknown);
    assert!(offline.live.is_none());
    let view = section_view(
        McpPanelSection::Status,
        Some(&offline),
        McpListingSupport::CURRENT,
    );
    assert_eq!(view.state, McpSectionState::NoEvidence);
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("no live registry evidence")),
        "{view:?}"
    );

    let (_owner, snapshot) = live_snapshot(
        "example",
        advertising_everything(),
        McpContributionCounts {
            tools: 1,
            resources: 0,
            prompts: 0,
        },
    );
    let connected = build_rows(
        &[status(stdio("example"), McpHealth::Reachable)],
        Some(snapshot.as_ref()),
    );
    let view = section_view(
        McpPanelSection::Status,
        connected.first(),
        McpListingSupport::CURRENT,
    );
    assert_eq!(view.state, McpSectionState::Live);
    assert!(
        view.lines.iter().any(|line| line.contains("ready")),
        "{view:?}"
    );
}

#[test]
fn an_http_target_is_rendered_without_its_query_or_userinfo() {
    let rendered = display_target(
        McpTransportKind::StreamableHttp,
        &format!("https://user:{URL_SECRET}@mcp.example.test/v1?token={URL_SECRET}#frag"),
    );
    assert!(!rendered.contains(URL_SECRET), "{rendered}");
    assert!(
        rendered.contains("https://mcp.example.test/v1"),
        "{rendered}"
    );
    assert!(rendered.contains("elided"), "{rendered}");
    assert_eq!(
        display_target(
            McpTransportKind::StreamableHttp,
            "https://mcp.example.test/v1"
        ),
        "https://mcp.example.test/v1"
    );
    assert_eq!(
        display_target(McpTransportKind::Stdio, "/usr/bin/example-mcp --serve"),
        "/usr/bin/example-mcp --serve"
    );
}

/// A URL that could carry a secret — a query string or userinfo — never enters
/// the store at all, so no panel line can ever render one. The row that does
/// render carries only the credential-free endpoint.
#[test]
fn no_rendered_panel_line_carries_a_url_secret() {
    assert!(
        StoredServer::new(
            "remote",
            McpTransportKind::StreamableHttp,
            format!("https://mcp.example.test/v1?token={URL_SECRET}"),
        )
        .is_err(),
        "a URL with a query string is refused at add time"
    );
    assert!(
        StoredServer::new(
            "remote",
            McpTransportKind::StreamableHttp,
            format!("https://user:{URL_SECRET}@mcp.example.test/v1"),
        )
        .is_err(),
        "a URL with userinfo is refused at add time"
    );
    let mut server = StoredServer::new(
        "remote",
        McpTransportKind::StreamableHttp,
        "https://mcp.example.test/v1",
    )
    .unwrap();
    server.enabled = false;
    let mut panel = McpPanelView::new(
        build_rows(&[status(server, McpHealth::AuthorizationRequired)], None),
        McpListingSupport::CURRENT,
    );
    for section in McpPanelSection::ALL {
        assert_eq!(panel.section(), section);
        for line in panel.lines() {
            assert!(!line.text.contains(URL_SECRET), "{line:?}");
        }
        press(&mut panel, KeyCode::Tab);
    }
}

#[test]
fn a_refused_operation_reports_the_reason_without_echoing_the_typed_target() {
    let store = TestStore::with(vec![stdio("github")]);
    let management = management(&store);
    let mut panel = McpPanelView::new(
        build_rows(&[status(stdio("github"), McpHealth::Unknown)], None),
        McpListingSupport::CURRENT,
    );
    open_actions(&mut panel, McpOperation::Add);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Handled
    );
    assert!(panel.is_prompting());
    for character in "github".chars() {
        press(&mut panel, KeyCode::Char(character));
    }
    press(&mut panel, KeyCode::Tab);
    press(&mut panel, KeyCode::Tab);
    for character in format!("/bin/x --token {URL_SECRET}").chars() {
        press(&mut panel, KeyCode::Char(character));
    }
    let McpPanelKeyOutcome::Run(intent) = press(&mut panel, KeyCode::Enter) else {
        panic!("submitting the form must produce an add intent");
    };
    let error = dispatch(&management, &intent).expect_err("duplicate names are refused");
    assert!(error.contains("already exists"), "{error}");
    assert!(!error.contains(URL_SECRET), "{error}");
    panel.note_failure(error);
    for line in panel.lines() {
        assert!(!line.text.contains(URL_SECRET), "{line:?}");
    }
    assert_eq!(store.names(), vec!["github".to_owned()]);
}

#[test]
fn every_operation_is_carried_out_by_the_shared_management_layer() {
    let store = TestStore::with(vec![stdio("github")]);
    let management = McpManagement::new(store.clone())
        .with_probe(Arc::new(FixedProbe(McpHealth::AuthorizationRequired)));

    assert!(matches!(
        dispatch(
            &management,
            &McpPanelIntent::Add {
                name: "linear".to_owned(),
                transport: McpTransportKind::StreamableHttp,
                target: "https://mcp.example.test/v1".to_owned(),
            }
        ),
        Ok(McpPanelOutcome::Committed(_))
    ));
    assert_eq!(
        store.names(),
        vec!["github".to_owned(), "linear".to_owned()]
    );

    let McpPanelOutcome::Listed(rows) = dispatch(&management, &McpPanelIntent::List).unwrap()
    else {
        panic!("list answers with rows");
    };
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.health == McpHealth::Unknown),
        "opening the panel reads the store; it does not connect to two servers"
    );

    let McpPanelOutcome::Listed(rows) = dispatch(&management, &McpPanelIntent::ListProbed).unwrap()
    else {
        panic!("the probing list answers with rows");
    };
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.health == McpHealth::AuthorizationRequired),
        "and the explicit refresh does connect"
    );

    for intent in [
        McpPanelIntent::Auth {
            name: "github".to_owned(),
        },
        McpPanelIntent::Test {
            name: "github".to_owned(),
        },
    ] {
        assert_eq!(
            dispatch(&management, &intent).unwrap(),
            McpPanelOutcome::Health {
                name: "github".to_owned(),
                health: McpHealth::AuthorizationRequired,
            }
        );
    }

    dispatch(
        &management,
        &McpPanelIntent::Edit {
            name: "github".to_owned(),
            transport: McpTransportKind::Stdio,
            target: "/usr/bin/other".to_owned(),
        },
    )
    .unwrap();
    assert_eq!(store.get("github").unwrap().target, "/usr/bin/other");

    dispatch(
        &management,
        &McpPanelIntent::Enable {
            name: "github".to_owned(),
            on: false,
        },
    )
    .unwrap();
    assert!(!store.get("github").unwrap().enabled);

    dispatch(
        &management,
        &McpPanelIntent::Remove {
            name: "linear".to_owned(),
        },
    )
    .unwrap();
    assert_eq!(store.names(), vec!["github".to_owned()]);
}

#[test]
fn the_enable_action_reads_as_its_effect_and_carries_the_opposite_state() {
    let enabled = row(stdio("github"), McpHealth::Unknown);
    let mut disabled_server = stdio("github");
    disabled_server.enabled = false;
    let disabled = row(disabled_server, McpHealth::Unknown);
    assert_eq!(
        build_actions(Some(&enabled))
            .into_iter()
            .find(|action| action.operation == McpOperation::Enable)
            .map(|action| action.label),
        Some("Disable")
    );
    assert_eq!(
        build_actions(Some(&disabled))
            .into_iter()
            .find(|action| action.operation == McpOperation::Enable)
            .map(|action| action.label),
        Some("Enable")
    );

    let mut panel = McpPanelView::new(vec![enabled], McpListingSupport::CURRENT);
    open_actions(&mut panel, McpOperation::Enable);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Run(McpPanelIntent::Enable {
            name: "github".to_owned(),
            on: false,
        })
    );
}

#[test]
fn removing_a_server_requires_an_explicit_confirmation_that_defaults_to_cancel() {
    let mut panel = McpPanelView::new(
        vec![row(stdio("github"), McpHealth::Unknown)],
        McpListingSupport::CURRENT,
    );
    open_actions(&mut panel, McpOperation::Remove);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Handled
    );
    assert!(panel.is_prompting());
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Handled
    );
    assert!(!panel.is_prompting());

    open_actions(&mut panel, McpOperation::Remove);
    press(&mut panel, KeyCode::Enter);
    press(&mut panel, KeyCode::Right);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Run(McpPanelIntent::Remove {
            name: "github".to_owned(),
        })
    );
}

#[test]
fn a_long_server_list_is_windowed_visibly_rather_than_clipped_off_the_card() {
    let servers = (0..20)
        .map(|index| row(stdio(&format!("server{index:02}")), McpHealth::Unknown))
        .collect::<Vec<_>>();
    let mut panel = McpPanelView::new(servers, McpListingSupport::CURRENT);
    // Only the projected row lines carry a two-digit server name; the heading
    // and the key hint mention "server" without naming one.
    let rendered = |panel: &McpPanelView| {
        panel
            .lines()
            .into_iter()
            .filter(|line| (0..20).any(|index| line.text.contains(&format!("server{index:02}"))))
            .map(|line| line.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(rendered(&panel).len(), 8);
    assert!(
        panel
            .lines()
            .iter()
            .any(|line| line.text.contains("showing 1-8 of 20")),
        "{:?}",
        panel.lines()
    );

    for _ in 0..19 {
        press(&mut panel, KeyCode::Down);
    }
    assert_eq!(panel.selected(), 19);
    let visible = rendered(&panel);
    assert_eq!(visible.len(), 8);
    assert!(
        visible.iter().any(|line| line.contains("server19")),
        "{visible:?}"
    );
    assert!(
        panel
            .lines()
            .iter()
            .any(|line| line.text.contains("showing 13-20 of 20")),
        "{:?}",
        panel.lines()
    );
}

#[test]
fn height_bounded_lines_keep_selection_prompts_errors_and_hints_reachable() {
    let servers = (0..20)
        .map(|index| row(stdio(&format!("server{index:02}")), McpHealth::Unknown))
        .collect::<Vec<_>>();
    let mut panel = McpPanelView::new(servers, McpListingSupport::CURRENT);
    for _ in 0..19 {
        press(&mut panel, KeyCode::Down);
    }
    open_actions(&mut panel, McpOperation::Remove);

    let short = panel.lines_for_height(6);
    assert_eq!(short.len(), 6);
    assert!(short.iter().any(|line| line.text.contains("server19")));
    assert!(short.iter().any(|line| line.text.contains("Remove")));
    assert!(short.last().unwrap().text.contains("esc close"));

    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Handled
    );
    let confirm = panel.lines_for_height(3);
    assert_eq!(confirm.len(), 3);
    assert!(
        confirm
            .iter()
            .any(|line| line.text.contains("Remove `server19`"))
    );
    assert!(confirm.iter().any(|line| line.text.contains("[cancel]")));
    assert!(confirm.last().unwrap().text.contains("esc cancel"));

    press(&mut panel, KeyCode::Esc);
    open_actions(&mut panel, McpOperation::Add);
    press(&mut panel, KeyCode::Enter);
    panel.paste("local");
    press(&mut panel, KeyCode::Tab);
    press(&mut panel, KeyCode::Tab);
    panel.paste("/usr/bin/local");
    let form = panel.lines_for_height(5);
    assert_eq!(form.len(), 5);
    assert!(
        form.iter()
            .any(|line| line.text.contains("name") && line.text.contains("local"))
    );
    assert!(form.iter().any(|line| line.text.contains("transport")));
    assert!(
        form.iter()
            .any(|line| line.text.contains("command") && line.text.contains("/usr/bin/local"))
    );
    assert!(form.last().unwrap().text.contains("enter submit"));

    press(&mut panel, KeyCode::Esc);
    panel.note_failure("bounded refusal remains visible");
    let refused = panel.lines_for_height(4);
    assert_eq!(refused.len(), 4);
    assert!(refused.iter().any(|line| line.text.contains("server19")));
    assert!(
        refused
            .iter()
            .any(|line| line.text == "bounded refusal remains visible")
    );
    assert!(refused.last().unwrap().text.contains("esc close"));

    // A newly opened form must not carry a stale validation/refusal message
    // beside values the operator is now correcting.
    open_actions(&mut panel, McpOperation::Add);
    press(&mut panel, KeyCode::Enter);
    assert!(
        panel
            .lines_for_height(5)
            .iter()
            .all(|line| line.text != "bounded refusal remains visible")
    );
}

#[test]
fn mouse_selects_visible_controls_without_running_and_wheel_never_leaks() {
    let mut panel = McpPanelView::new(
        vec![
            row(stdio("alpha"), McpHealth::Unknown),
            row(stdio("beta"), McpHealth::Unknown),
        ],
        McpListingSupport::CURRENT,
    );
    let body = Rect::new(10, 5, 100, 20);
    panel.set_mouse_layout(body);
    assert_eq!(
        panel.handle_mouse(mouse(MouseEventKind::ScrollDown, body.x, body.y - 1)),
        McpPanelKeyOutcome::Handled
    );
    assert_eq!(panel.selected(), 0, "outside wheel movement is ignored");
    assert_eq!(
        panel.handle_mouse(mouse(MouseEventKind::ScrollDown, body.x, body.y + 1)),
        McpPanelKeyOutcome::Handled
    );
    assert_eq!(panel.selected(), 1, "inside wheel follows the server list");

    panel.set_mouse_layout(body);
    let lines = panel.lines_for_height(body.height);
    let tabs_index = lines
        .iter()
        .position(|line| line.text.contains("[status]") && line.text.contains("actions"))
        .unwrap();
    let action_column = lines[tabs_index].text.find("actions").unwrap();
    panel.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + u16::try_from(action_column).unwrap(),
        body.y + u16::try_from(tabs_index).unwrap(),
    ));
    assert_eq!(panel.section(), McpPanelSection::Actions);
    assert!(
        !panel.is_prompting(),
        "selecting a section executes nothing"
    );

    panel.set_mouse_layout(body);
    let lines = panel.lines_for_height(body.height);
    let remove_index = lines
        .iter()
        .position(|line| line.text.contains("Remove"))
        .unwrap();
    panel.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y + u16::try_from(remove_index).unwrap(),
    ));
    assert_eq!(
        panel.action_selected(),
        McpOperation::ALL
            .iter()
            .position(|operation| *operation == McpOperation::Remove)
            .unwrap()
    );
    assert!(!panel.is_prompting(), "selecting an action does not run it");

    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Handled
    );
    panel.set_mouse_layout(body);
    let lines = panel.lines_for_height(body.height);
    let choice_index = lines
        .iter()
        .position(|line| line.text.contains("[cancel]") && line.text.contains("remove"))
        .unwrap();
    let remove_column = lines[choice_index].text.rfind("remove").unwrap();
    panel.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + u16::try_from(remove_column).unwrap(),
        body.y + u16::try_from(choice_index).unwrap(),
    ));
    assert_eq!(
        panel.paste("remove\n/quit"),
        McpPanelKeyOutcome::Handled,
        "paste on confirmation is consumed without confirming"
    );
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Run(McpPanelIntent::Remove {
            name: "beta".to_owned(),
        }),
        "only an explicit Enter confirms the mouse-selected choice"
    );
}

#[test]
fn paste_enters_only_the_clicked_bounded_form_field_and_never_submits() {
    let mut panel = McpPanelView::new(
        vec![row(stdio("alpha"), McpHealth::Unknown)],
        McpListingSupport::CURRENT,
    );
    open_actions(&mut panel, McpOperation::Add);
    press(&mut panel, KeyCode::Enter);
    let body = Rect::new(3, 7, 100, 12);
    panel.set_mouse_layout(body);
    let lines = panel.lines_for_height(body.height);
    let target_index = lines
        .iter()
        .position(|line| line.text.contains("command"))
        .unwrap();
    panel.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y + u16::try_from(target_index).unwrap(),
    ));
    assert_eq!(
        panel.paste("\n/usr/bin/tool\r/quit\0"),
        McpPanelKeyOutcome::Handled
    );
    assert!(panel.is_prompting(), "paste must not submit the form");
    assert!(
        panel
            .lines()
            .iter()
            .any(|line| line.text.contains("/usr/bin/tool/quit"))
    );

    panel.set_mouse_layout(body);
    let lines = panel.lines_for_height(body.height);
    let name_index = lines
        .iter()
        .position(|line| line.text.contains("name"))
        .unwrap();
    panel.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y + u16::try_from(name_index).unwrap(),
    ));
    panel.paste(&format!("{}/bad\n", "a".repeat(100)));
    assert!(panel.is_prompting());
    let McpPanelKeyOutcome::Run(McpPanelIntent::Add { name, target, .. }) =
        press(&mut panel, KeyCode::Enter)
    else {
        panic!("explicit Enter should submit the still-open add form");
    };
    assert_eq!(name, "a".repeat(64));
    assert_eq!(target, "/usr/bin/tool/quit");
}

#[test]
fn tab_cycles_all_six_capabilities_and_wraps_in_both_directions() {
    let mut panel = McpPanelView::new(
        vec![row(stdio("github"), McpHealth::Unknown)],
        McpListingSupport::CURRENT,
    );
    let mut seen = Vec::new();
    for _ in 0..McpPanelSection::ALL.len() {
        seen.push(panel.section());
        press(&mut panel, KeyCode::Tab);
    }
    assert_eq!(seen, McpPanelSection::ALL.to_vec());
    assert_eq!(panel.section(), McpPanelSection::Status);
    press(&mut panel, KeyCode::BackTab);
    assert_eq!(panel.section(), McpPanelSection::Actions);
}

#[test]
fn an_unavailable_action_reports_its_refusal_instead_of_running() {
    let mut panel = McpPanelView::new(
        vec![row(managed("corp"), McpHealth::Reachable)],
        McpListingSupport::CURRENT,
    );
    open_actions(&mut panel, McpOperation::Remove);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        McpPanelKeyOutcome::Handled
    );
    assert!(!panel.is_prompting());
    assert!(
        panel.lines().iter().any(|line| {
            line.tone == McpPanelTone::Bad && line.text.contains("managed by an administrator")
        }),
        "{:?}",
        panel.lines()
    );
}

#[test]
fn the_panel_republishes_rows_only_from_a_store_read_after_the_operation_commits() {
    let store = TestStore::with(vec![stdio("github")]);
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(Arc::new(management(&store)), None);
    state.open_mcp_panel();
    assert_eq!(state.mcp_panel().expect("panel opened").rows().len(), 1);

    // Enter from any section jumps to actions with `add` first in the closed
    // order; the second Enter opens its form.
    state.handle_terminal_event(&key(KeyCode::Enter));
    state.handle_terminal_event(&key(KeyCode::Enter));
    for character in "linear".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character)));
    }
    state.handle_terminal_event(&key(KeyCode::Tab));
    state.handle_terminal_event(&key(KeyCode::Tab));
    for character in "/usr/bin/linear".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character)));
    }
    // Before the commit the panel still shows exactly what the store holds.
    assert_eq!(state.mcp_panel().expect("panel open").rows().len(), 1);
    state.handle_terminal_event(&key(KeyCode::Enter));

    let names = state
        .mcp_panel()
        .expect("panel stays open")
        .rows()
        .iter()
        .map(|row| row.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["github".to_owned(), "linear".to_owned()]);
    assert_eq!(store.names(), names);

    state.handle_terminal_event(&key(KeyCode::Esc));
    assert!(state.mcp_panel().is_none());
}

#[test]
fn app_routing_keeps_mcp_mouse_and_paste_inside_the_rendered_panel() {
    let store = TestStore::with(vec![stdio("alpha"), stdio("beta")]);
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_mcp_services(Arc::new(management(&store)), None);
    assert!(state.input.insert_str("preserved draft"));
    state.open_mcp_panel();

    // Drawing establishes the exact mouse hit map used by AppState. Select a
    // server and the actions tab through those rendered coordinates; neither
    // click is an activation gesture.
    let rows = render_rows(&mut state, 76, 22);
    let (beta_x, beta_y) = rendered_control(&rows, "beta");
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::ScrollDown,
        beta_x,
        beta_y,
    )));
    assert_eq!(state.mcp_panel().unwrap().selected(), 1);
    assert_eq!(state.input.lines(), ["preserved draft"]);
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::ScrollUp,
        beta_x,
        beta_y,
    )));
    assert_eq!(state.mcp_panel().unwrap().selected(), 0);
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        beta_x,
        beta_y,
    )));
    assert_eq!(state.mcp_panel().unwrap().selected(), 1);
    assert_eq!(store.names(), vec!["alpha".to_owned(), "beta".to_owned()]);

    let rows = render_rows(&mut state, 76, 22);
    let (actions_x, actions_y) = rendered_control(&rows, "actions");
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        actions_x,
        actions_y,
    )));
    assert_eq!(
        state.mcp_panel().unwrap().section(),
        McpPanelSection::Actions
    );
    assert_eq!(store.names(), vec!["alpha".to_owned(), "beta".to_owned()]);

    // Mouse only focuses Add. The explicit Enter opens its bounded form.
    let rows = render_rows(&mut state, 76, 22);
    let (add_x, add_y) = rendered_control(&rows, "Add");
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        add_x,
        add_y,
    )));
    assert!(!state.mcp_panel().unwrap().is_prompting());
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(state.mcp_panel().unwrap().is_prompting());

    // Paste follows only the clicked form field. Newlines/control input are
    // sanitized, the hidden composer remains byte-for-byte intact, and no
    // operation commits until a separate Enter.
    let rows = render_rows(&mut state, 76, 22);
    let (command_x, command_y) = rendered_control(&rows, "command");
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        command_x,
        command_y,
    )));
    state.handle_terminal_event(&Event::Paste("/usr/bin/local\n/quit\0".to_owned()));
    assert_eq!(state.input.lines(), ["preserved draft"]);
    assert_eq!(store.names(), vec!["alpha".to_owned(), "beta".to_owned()]);

    let rows = render_rows(&mut state, 76, 22);
    let (name_x, name_y) = rendered_control(&rows, "name");
    state.handle_terminal_event(&Event::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        name_x,
        name_y,
    )));
    state.handle_terminal_event(&Event::Paste("local".to_owned()));
    assert_eq!(state.input.lines(), ["preserved draft"]);
    assert_eq!(store.names(), vec!["alpha".to_owned(), "beta".to_owned()]);

    state.handle_terminal_event(&key(KeyCode::Enter));
    assert_eq!(
        store.names(),
        vec!["alpha".to_owned(), "beta".to_owned(), "local".to_owned()]
    );
    assert_eq!(store.get("local").unwrap().target, "/usr/bin/local/quit");
    assert_eq!(state.input.lines(), ["preserved draft"]);
}

#[test]
fn the_panel_contribution_registers_in_the_ui_panel_slot_and_disposes_with_its_context() {
    struct PanelPlugin;
    impl Plugin for PanelPlugin {
        fn name(&self) -> &'static str {
            "test-mcp-panel"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "test-mcp-panel",
                "1.0.0",
                &[heycode_core::PluginContributionKind::UserInterface],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_UI]
        }

        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            let ui = context
                .get::<UiRegistry>(SERVICE_UI)
                .ok_or_else(|| heycode_core::CoreError::other("ui missing"))?;
            ui.register(
                context,
                panel_descriptor()
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                Arc::new(()),
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    let plugins: Vec<Box<dyn Plugin>> = vec![ui_registry_plugin(), Box::new(PanelPlugin)];
    let mut context = compose(&plugins).unwrap();
    let ui = context.get::<UiRegistry>(SERVICE_UI).unwrap();
    let id = UiContributionId::new("mcp").unwrap();
    assert!(
        ui.snapshot()
            .unwrap()
            .iter()
            .any(|row| row.slot() == UiSlot::Panel && row.id() == &id)
    );
    context.shutdown();
    assert!(ui.snapshot().unwrap().is_empty());
}

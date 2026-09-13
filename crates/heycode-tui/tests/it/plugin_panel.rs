//! U13 plugin panel projection, honesty, action and disposal contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use heycode_core::{Context, Plugin, compose};
use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginOperation, PluginState,
    PluginStateStore,
};
use heycode_extensions::{
    ApiVersion, Architecture, ManifestValidator, OperatingSystem, PlatformTarget, PluginId,
    PluginManifest, PluginPermission, PluginVersion,
};
use heycode_tui::app::AppState;
use heycode_tui::plugin_panel::{
    PluginPackageIndex, PluginPanelIntent, PluginPanelKeyOutcome, PluginPanelOutcome,
    PluginPanelRow, PluginPanelSection, PluginPanelTone, PluginPanelView, PluginSectionState,
    RollbackState, build_actions, build_rows, dispatch, list, panel_descriptor, section_view,
};
use heycode_tui::render::draw;
use heycode_ui::{SERVICE_UI, UiContributionId, UiRegistry, UiSlot, ui_registry_plugin};
use ratatui::{Terminal, backend::TestBackend};

const HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct TestStore {
    states: Mutex<BTreeMap<String, PluginState>>,
}

impl TestStore {
    fn with(states: Vec<PluginState>) -> Arc<Self> {
        Arc::new(Self {
            states: Mutex::new(
                states
                    .into_iter()
                    .map(|state| (state.id.as_str().to_owned(), state))
                    .collect(),
            ),
        })
    }

    fn ids(&self) -> Vec<String> {
        self.states.lock().unwrap().keys().cloned().collect()
    }

    fn get(&self, id: &str) -> Option<PluginState> {
        self.states.lock().unwrap().get(id).cloned()
    }
}

impl PluginStateStore for TestStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.states.lock().unwrap().clone())
    }

    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.states.lock().unwrap() = states.clone();
        Ok(())
    }
}

struct TestCache(Vec<PluginVersion>);

impl InstalledVersions for TestCache {
    fn versions(&self, _id: &PluginId) -> Vec<PluginVersion> {
        self.0.clone()
    }
}

fn id(value: &str) -> PluginId {
    PluginId::new(value.to_owned()).unwrap()
}

fn version(value: &str) -> PluginVersion {
    PluginVersion::parse(value.to_owned()).unwrap()
}

fn state(name: &str, active: &str, previous: Option<&str>, enabled: bool) -> PluginState {
    PluginState {
        id: id(name),
        active: version(active),
        previous: previous.map(version),
        enabled,
    }
}

/// A manifest built through the real PL01 validator, so the projection is
/// exercised against admitted metadata rather than a hand-made struct.
fn manifest(permissions: &str, default_enabled: bool, credentials: bool) -> PluginManifest {
    let authentication = if credentials {
        "policy = \"required\"\ncredentials = [{ reference = \"acme/quality/api\", kind = \"api-key\" }]"
    } else {
        "policy = \"none\"\ncredentials = []"
    };
    let toml = format!(
        r#"
schema_version = 1
id = "acme/quality"
name = "Quality"
version = "1.2.0"
description = "A test package."
license = "MIT"
default_enabled = {default_enabled}
requested_permissions = [{permissions}]
configuration_schema = "schemas/config.schema.json"
platforms = [{{ os = "macos", architecture = "aarch64" }}]
dependencies = []
conflicts = []

[api]
minimum = 1
maximum = 1

[source]
kind = "https"
locator = "https://plugins.example.test/acme/quality.tar.zst"
checksum = "{HASH}"
signature = {{ algorithm = "ed25519", key_id = "acme/releases", value = "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYQ==" }}
update_channel = "stable"

[authentication]
{authentication}

[[contributions]]
kind = "skill"
id = "review"
path = "skills/review/SKILL.md"
exposure = {{ mode = "namespaced" }}
"#
    );
    ManifestValidator::new(
        ApiVersion::new(1).unwrap(),
        PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
    )
    .validate_toml(&toml)
    .unwrap()
}

fn index_with_manifest(manifest: &PluginManifest) -> PluginPackageIndex {
    let mut index = PluginPackageIndex::new();
    index.add_package(&id("acme/quality"), &version("1.2.0"), HASH, 1, false);
    index.add_manifest(manifest);
    index
}

fn summary_only_index() -> PluginPackageIndex {
    let mut index = PluginPackageIndex::new();
    index.add_package(&id("acme/quality"), &version("1.2.0"), HASH, 3, true);
    index
}

fn row(states: &[PluginState], packages: Option<&PluginPackageIndex>) -> PluginPanelRow {
    build_rows(states, packages).pop().expect("one row")
}

fn press(panel: &mut PluginPanelView, code: KeyCode) -> PluginPanelKeyOutcome {
    panel.handle_key(code, KeyModifiers::NONE)
}

/// Move the cursor onto `operation`, bounded by the closed sets it walks so an
/// unreachable section or action fails by name instead of spinning forever.
fn open_actions(panel: &mut PluginPanelView, operation: PluginOperation) {
    press(panel, KeyCode::Right);
    let index = PluginOperation::ALL
        .iter()
        .position(|candidate| *candidate == operation)
        .expect("operation is in the closed set");
    for _ in 0..PluginOperation::ALL.len() {
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
fn panel_offers_every_pl06_operation_exactly_once_in_the_closed_order() {
    let selected = row(&[state("acme/quality", "1.2.0", None, true)], None);
    let offered = build_actions(Some(&selected))
        .into_iter()
        .map(|action| action.operation)
        .collect::<Vec<_>>();
    assert_eq!(offered, PluginOperation::ALL.to_vec());
    assert_eq!(offered.len(), 6);
}

#[test]
fn refresh_is_a_query_and_never_appears_among_the_six_operations() {
    let store = TestStore::with(vec![state("acme/quality", "1.2.0", None, true)]);
    let lifecycle =
        PluginLifecycle::new(store.clone(), Arc::new(TestCache(vec![version("1.2.0")])));
    assert!(matches!(
        list(&lifecycle),
        Ok(PluginPanelOutcome::Listed(states)) if states.len() == 1
    ));
    let selected = row(&[state("acme/quality", "1.2.0", None, true)], None);
    for action in build_actions(Some(&selected)) {
        assert_ne!(action.label, "Refresh");
        assert!(!format!("{:?}", action.operation).contains("List"));
    }
}

#[test]
fn an_unresolved_manifest_reports_permissions_unknown_and_never_as_none() {
    let projected = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    );
    let view = section_view(PluginPanelSection::Permissions, Some(&projected));
    assert_eq!(view.state, PluginSectionState::NoEvidence);
    assert!(
        view.lines.iter().any(|line| line.contains("unknown")),
        "{view:?}"
    );
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("not the same as none")),
        "{view:?}"
    );
    assert!(
        !view
            .lines
            .iter()
            .any(|line| line.contains("requests no host permissions")),
        "an uninspected package must never read as requesting nothing: {view:?}"
    );
}

#[test]
fn a_manifest_requesting_no_permissions_is_distinct_from_an_unresolved_one() {
    let empty = manifest("", false, false);
    let projected = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&index_with_manifest(&empty)),
    );
    let view = section_view(PluginPanelSection::Permissions, Some(&projected));
    assert_eq!(view.state, PluginSectionState::NotDeclared);
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("requests no host permissions")),
        "{view:?}"
    );
    assert!(!view.lines.iter().any(|line| line.contains("unknown")));
}

#[test]
fn permissions_render_every_capability_the_manifest_requests() {
    let requested = manifest(
        r#""filesystem_write", "process_spawn", "network_access", "credential_use""#,
        false,
        true,
    );
    let projected = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&index_with_manifest(&requested)),
    );
    let view = section_view(PluginPanelSection::Permissions, Some(&projected));
    assert_eq!(view.state, PluginSectionState::Live);
    for permission in [
        PluginPermission::FilesystemWrite,
        PluginPermission::ProcessSpawn,
        PluginPermission::NetworkAccess,
    ] {
        assert!(
            view.lines
                .iter()
                .any(|line| line.contains(permission.as_str())),
            "{permission:?} missing from {view:?}"
        );
    }
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("credentials required")),
        "{view:?}"
    );
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("acme/quality/api")),
        "{view:?}"
    );
}

#[test]
fn provenance_shows_origin_channel_checksum_and_signature_state() {
    let package = manifest(r#""filesystem_read""#, false, false);
    let projected = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&index_with_manifest(&package)),
    );
    let view = section_view(PluginPanelSection::Provenance, Some(&projected));
    assert_eq!(view.state, PluginSectionState::Live);
    for expected in [
        "https archive",
        "https://plugins.example.test/acme/quality.tar.zst",
        "stable",
        HASH,
        "signature: attached",
    ] {
        assert!(
            view.lines.iter().any(|line| line.contains(expected)),
            "{expected} missing from {view:?}"
        );
    }
}

#[test]
fn provenance_is_unknown_without_a_resolved_manifest() {
    let projected = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    );
    let view = section_view(PluginPanelSection::Provenance, Some(&projected));
    assert_eq!(view.state, PluginSectionState::NoEvidence);
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("origin is unknown")),
        "{view:?}"
    );
    // The summary level is still real evidence and must still be shown.
    assert!(
        view.lines.iter().any(|line| line.contains(HASH)),
        "{view:?}"
    );
}

#[test]
fn a_pruned_rollback_target_is_refused_before_it_is_chosen() {
    let mut pruned = PluginPackageIndex::new();
    pruned.add_package(&id("acme/quality"), &version("1.2.0"), HASH, 1, false);
    let projected = row(
        &[state("acme/quality", "1.2.0", Some("1.1.0"), true)],
        Some(&pruned),
    );
    assert_eq!(
        projected.rollback,
        RollbackState::PreviousVersionPruned(version("1.1.0"))
    );
    let rollback = build_actions(Some(&projected))
        .into_iter()
        .find(|action| action.operation == PluginOperation::Rollback)
        .unwrap();
    assert!(!rollback.is_available());
    assert!(
        rollback
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("no longer in the package cache"),
        "{rollback:?}"
    );
}

#[test]
fn rollback_is_offered_only_when_the_cache_still_holds_the_previous_version() {
    let mut held = PluginPackageIndex::new();
    held.add_package(&id("acme/quality"), &version("1.2.0"), HASH, 1, false);
    held.add_package(&id("acme/quality"), &version("1.1.0"), HASH, 1, false);
    let available = row(
        &[state("acme/quality", "1.2.0", Some("1.1.0"), true)],
        Some(&held),
    );
    assert_eq!(
        available.rollback,
        RollbackState::Available(version("1.1.0"))
    );
    assert!(
        build_actions(Some(&available))
            .into_iter()
            .find(|action| action.operation == PluginOperation::Rollback)
            .unwrap()
            .is_available()
    );

    let fresh = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    );
    assert_eq!(fresh.rollback, RollbackState::NoPreviousVersion);

    let uncached = row(&[state("acme/quality", "1.2.0", Some("1.1.0"), true)], None);
    assert_eq!(
        uncached.rollback,
        RollbackState::CacheUnknown(version("1.1.0"))
    );
    assert!(
        !build_actions(Some(&uncached))
            .into_iter()
            .find(|action| action.operation == PluginOperation::Rollback)
            .unwrap()
            .is_available()
    );
}

#[test]
fn enable_and_disable_are_offered_by_current_state_never_both_at_once() {
    for enabled in [true, false] {
        let projected = row(
            &[state("acme/quality", "1.2.0", None, enabled)],
            Some(&summary_only_index()),
        );
        let actions = build_actions(Some(&projected));
        let enable = actions
            .iter()
            .find(|action| action.operation == PluginOperation::Enable)
            .unwrap();
        let disable = actions
            .iter()
            .find(|action| action.operation == PluginOperation::Disable)
            .unwrap();
        assert_eq!(enable.is_available(), !enabled);
        assert_eq!(disable.is_available(), enabled);
    }
}

#[test]
fn the_enable_section_separates_the_manifest_request_from_the_host_decision() {
    // Manifest asks to be enabled by default; the host has it disabled.
    let projected = row(
        &[state("acme/quality", "1.2.0", None, false)],
        Some(&summary_only_index()),
    );
    let view = section_view(PluginPanelSection::Enable, Some(&projected));
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("state: disabled")),
        "{view:?}"
    );
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("manifest asks to be enabled by default")),
        "{view:?}"
    );

    let unknown = row(&[state("acme/quality", "1.2.0", None, false)], None);
    let view = section_view(PluginPanelSection::Enable, Some(&unknown));
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("default-enable request is unknown")),
        "{view:?}"
    );
}

#[test]
fn cached_versions_are_listed_by_descending_precedence() {
    let mut index = PluginPackageIndex::new();
    for raw in ["1.1.0", "2.0.0", "1.10.0", "1.2.0"] {
        index.add_package(&id("acme/quality"), &version(raw), HASH, 1, false);
    }
    let projected = row(&[state("acme/quality", "1.2.0", None, true)], Some(&index));
    assert_eq!(
        projected
            .cached_versions
            .iter()
            .map(PluginVersion::as_str)
            .collect::<Vec<_>>(),
        ["2.0.0", "1.10.0", "1.2.0", "1.1.0"]
    );
}

#[test]
fn every_operation_is_carried_out_by_the_shared_lifecycle_layer() {
    let store = TestStore::with(vec![state("acme/quality", "1.2.0", None, true)]);
    let cache = Arc::new(TestCache(vec![
        version("1.2.0"),
        version("1.3.0"),
        version("2.0.0"),
    ]));
    let lifecycle = PluginLifecycle::new(store.clone(), cache);

    dispatch(
        &lifecycle,
        &PluginPanelIntent::Install {
            id: id("acme/other"),
            version: version("2.0.0"),
        },
    )
    .unwrap();
    assert_eq!(
        store.ids(),
        vec!["acme/other".to_owned(), "acme/quality".to_owned()]
    );

    dispatch(
        &lifecycle,
        &PluginPanelIntent::SetEnabled {
            id: id("acme/quality"),
            enabled: false,
        },
    )
    .unwrap();
    assert!(!store.get("acme/quality").unwrap().enabled);

    dispatch(
        &lifecycle,
        &PluginPanelIntent::Update {
            id: id("acme/quality"),
            version: version("1.3.0"),
        },
    )
    .unwrap();
    assert_eq!(store.get("acme/quality").unwrap().active, version("1.3.0"));
    assert_eq!(
        store.get("acme/quality").unwrap().previous,
        Some(version("1.2.0"))
    );

    assert_eq!(
        dispatch(
            &lifecycle,
            &PluginPanelIntent::Rollback {
                id: id("acme/quality")
            }
        )
        .unwrap(),
        PluginPanelOutcome::RolledBack {
            id: "acme/quality".to_owned(),
            restored: version("1.2.0"),
        }
    );
    assert_eq!(store.get("acme/quality").unwrap().active, version("1.2.0"));

    dispatch(
        &lifecycle,
        &PluginPanelIntent::Remove {
            id: id("acme/other"),
        },
    )
    .unwrap();
    assert_eq!(store.ids(), vec!["acme/quality".to_owned()]);
}

#[test]
fn a_malformed_form_entry_is_reported_where_it_was_typed() {
    let mut panel = PluginPanelView::new(vec![row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    )]);
    open_actions(&mut panel, PluginOperation::Install);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        PluginPanelKeyOutcome::Handled
    );
    assert!(panel.is_prompting());
    for character in "NOT A VALID ID".chars() {
        press(&mut panel, KeyCode::Char(character));
    }
    press(&mut panel, KeyCode::Tab);
    for character in "1.0.0".chars() {
        press(&mut panel, KeyCode::Char(character));
    }
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        PluginPanelKeyOutcome::Handled
    );
    assert!(
        panel.lines().iter().any(|line| {
            line.tone == PluginPanelTone::Bad && line.text.contains("invalid plugin id")
        }),
        "{:?}",
        panel.lines()
    );
    assert!(panel.is_prompting(), "the form stays open to be corrected");
    let visual = panel.lines_for_height(17);
    assert!(
        visual
            .iter()
            .any(|line| line.text.contains("invalid plugin id")),
        "{visual:?}"
    );
    assert!(
        visual.iter().any(|line| line.text.contains("esc cancel")),
        "{visual:?}"
    );
}

#[test]
fn removing_a_plugin_requires_an_explicit_confirmation_that_defaults_to_cancel() {
    let mut panel = PluginPanelView::new(vec![row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    )]);
    open_actions(&mut panel, PluginOperation::Remove);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        PluginPanelKeyOutcome::Handled
    );
    assert!(panel.is_prompting());
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        PluginPanelKeyOutcome::Handled
    );
    assert!(!panel.is_prompting());

    open_actions(&mut panel, PluginOperation::Remove);
    press(&mut panel, KeyCode::Enter);
    press(&mut panel, KeyCode::Right);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        PluginPanelKeyOutcome::Run(PluginPanelIntent::Remove {
            id: id("acme/quality")
        })
    );
}

#[test]
fn an_unavailable_action_reports_its_refusal_instead_of_running() {
    let mut panel = PluginPanelView::new(vec![row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    )]);
    open_actions(&mut panel, PluginOperation::Rollback);
    assert_eq!(
        press(&mut panel, KeyCode::Enter),
        PluginPanelKeyOutcome::Handled
    );
    assert!(
        panel.lines().iter().any(|line| {
            line.tone == PluginPanelTone::Bad && line.text.contains("no previous version")
        }),
        "{:?}",
        panel.lines()
    );
}

#[test]
fn tab_cycles_all_four_capabilities_and_wraps_in_both_directions() {
    let mut panel = PluginPanelView::new(vec![row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&summary_only_index()),
    )]);
    let mut seen = Vec::new();
    for _ in 0..PluginPanelSection::ALL.len() {
        seen.push(panel.section());
        press(&mut panel, KeyCode::Tab);
    }
    assert_eq!(seen, PluginPanelSection::ALL.to_vec());
    assert_eq!(panel.section(), PluginPanelSection::Provenance);
    press(&mut panel, KeyCode::BackTab);
    assert_eq!(panel.section(), PluginPanelSection::Update);
}

#[test]
fn a_long_plugin_list_is_windowed_visibly_rather_than_clipped_off_the_card() {
    let states = (0..20)
        .map(|index| state(&format!("acme/plugin{index:02}"), "1.0.0", None, true))
        .collect::<Vec<_>>();
    let mut panel = PluginPanelView::new(build_rows(&states, None));
    let rendered = |panel: &PluginPanelView| {
        panel
            .lines()
            .into_iter()
            .filter(|line| {
                line.text
                    .trim_start()
                    .trim_start_matches("● ")
                    .starts_with("acme/plugin")
            })
            .count()
    };
    assert_eq!(rendered(&panel), 8);
    assert!(
        panel
            .lines()
            .iter()
            .any(|line| line.text.contains("showing 1-8 of 20"))
    );
    for _ in 0..19 {
        press(&mut panel, KeyCode::Down);
    }
    assert_eq!(panel.selected(), 19);
    assert_eq!(rendered(&panel), 8);
    assert!(
        panel
            .lines()
            .iter()
            .any(|line| line.text.contains("showing 13-20 of 20"))
    );
}

#[test]
fn bounded_visual_pages_section_facts_without_hiding_actions_or_the_hint() {
    let package = manifest(
        r#""filesystem_write", "process_spawn", "network_access", "credential_use""#,
        false,
        true,
    );
    let projected = row(
        &[state("acme/quality", "1.2.0", None, true)],
        Some(&index_with_manifest(&package)),
    );
    let mut panel = PluginPanelView::new(vec![projected]);

    let first = panel.lines_for_height(12);
    assert_eq!(first.len(), 12, "{first:?}");
    for expected in [
        "acme/quality",
        "Install",
        "Enable",
        "Disable",
        "Update",
        "Roll back",
        "Remove",
        "PgUp/PgDn",
        "Esc",
    ] {
        assert!(
            first.iter().any(|line| line.text.contains(expected)),
            "{expected} missing from {first:?}"
        );
    }
    press(&mut panel, KeyCode::PageDown);
    press(&mut panel, KeyCode::PageDown);
    let last = panel.lines_for_height(12);
    assert!(
        last.iter()
            .any(|line| line.text.contains("signature: attached")),
        "{last:?}"
    );

    press(&mut panel, KeyCode::Tab);
    let permissions = panel.lines_for_height(17);
    for expected in [
        "requests filesystem_write",
        "requests process_spawn",
        "requests network_access",
        "requests credential_use",
        "credentials required",
        "credential reference: acme/quality/api",
    ] {
        assert!(
            permissions.iter().any(|line| line.text.contains(expected)),
            "{expected} missing from {permissions:?}"
        );
    }
}

#[test]
fn the_panel_republishes_rows_only_from_a_store_read_after_the_operation_commits() {
    let store = TestStore::with(vec![state("acme/quality", "1.2.0", None, true)]);
    let lifecycle = PluginLifecycle::new(
        store.clone(),
        Arc::new(TestCache(vec![version("1.2.0"), version("1.3.0")])),
    );
    let mut app = AppState::new("model", std::path::PathBuf::from("/workspace"));
    app.set_plugin_services(Arc::new(lifecycle), None);
    app.open_plugin_panel();
    assert_eq!(app.plugin_panel().expect("panel opened").rows().len(), 1);
    assert!(app.plugin_panel().unwrap().rows()[0].enabled);

    // Details initially select the action that toggles the current state.
    app.handle_terminal_event(&key(KeyCode::Right));
    assert!(app.plugin_panel().unwrap().rows()[0].enabled);
    app.handle_terminal_event(&key(KeyCode::Enter));

    assert!(!app.plugin_panel().expect("panel stays open").rows()[0].enabled);
    assert!(!store.get("acme/quality").unwrap().enabled);

    app.handle_terminal_event(&key(KeyCode::Esc));
    assert!(app.plugin_panel().is_some());
    app.handle_terminal_event(&key(KeyCode::Esc));
    assert!(app.plugin_panel().is_none());
}

#[test]
fn plugin_panel_consumes_paste_and_mouse_wheel_tracks_the_focused_list_at_narrow_size() {
    let states = (0..20)
        .map(|index| state(&format!("acme/plugin{index:02}"), "1.0.0", None, true))
        .collect::<Vec<_>>();
    let lifecycle = Arc::new(PluginLifecycle::new(
        TestStore::with(states),
        Arc::new(TestCache(vec![version("1.0.0")])),
    ));
    let mut app = AppState::new("model", std::path::PathBuf::from("/workspace"));
    app.set_plugin_services(lifecycle, None);
    assert!(app.input.insert_str("preserved draft"));
    app.open_plugin_panel();

    app.handle_terminal_event(&Event::Paste("remove everything\n".to_owned()));
    assert_eq!(app.input.lines(), ["preserved draft"]);
    app.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.plugin_panel().unwrap().selected(), 1);
    app.handle_terminal_event(&key(KeyCode::Right));
    app.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 20,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.plugin_panel().unwrap().action_selected(), 3);

    let rendered = frame_text_at(&mut app, 60, 30);
    assert!(rendered.contains("Installed"), "{rendered}");
    assert!(rendered.contains("acme/plugin01"), "{rendered}");
    assert!(!rendered.contains("checksum"), "{rendered}");
    assert!(rendered.contains("Roll back"), "{rendered}");
    assert!(rendered.contains("Remove"), "{rendered}");
    assert!(rendered.contains("i for details"), "{rendered}");
    app.handle_terminal_event(&key(KeyCode::Esc));
    assert!(app.plugin_panel().is_some());
    app.handle_terminal_event(&key(KeyCode::Esc));
    assert!(app.plugin_panel().is_none());
    assert_eq!(app.input.lines(), ["preserved draft"]);
}

#[test]
fn the_panel_contribution_registers_in_the_ui_panel_slot_and_disposes_with_its_context() {
    struct PanelPlugin;
    impl Plugin for PanelPlugin {
        fn name(&self) -> &'static str {
            "test-plugin-panel"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "test-plugin-panel",
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
    let identity = UiContributionId::new("plugins").unwrap();
    assert!(
        ui.snapshot()
            .unwrap()
            .iter()
            .any(|entry| entry.slot() == UiSlot::Panel && entry.id() == &identity)
    );
    context.shutdown();
    assert!(ui.snapshot().unwrap().is_empty());
}

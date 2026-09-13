//! U08 inference-vs-runtime discovery, rendering and interaction contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::{CapabilitySupport, ProviderDescriptor, ProviderProfile};
use heycode_runtime::{AgentRuntimeDescriptor, AgentRuntimeKind, RuntimeCapabilities};
use heycode_tui::app::{AppState, WelcomeStatusView};
use heycode_tui::render::draw;
use heycode_tui::route_picker::{
    RoutePickerClass, RoutePickerFilter, RoutePickerSelection, build_route_rows, filter_routes,
};
use ratatui::{Terminal, backend::TestBackend};

fn provider(id: &str, display: &str, model: &str) -> ProviderProfile {
    ProviderProfile {
        registry_name: id.to_owned(),
        descriptor: ProviderDescriptor {
            id: id.to_owned(),
            display_name: display.to_owned(),
            protocols: Vec::new(),
        },
        default_model: model.to_owned(),
        credential_reference: None,
    }
}

fn runtime(id: &str, display: &str, kind: AgentRuntimeKind) -> AgentRuntimeDescriptor {
    AgentRuntimeDescriptor::new(
        id,
        display,
        kind,
        RuntimeCapabilities {
            models: CapabilitySupport::Supported,
            resume: CapabilitySupport::Supported,
            ..RuntimeCapabilities::unknown()
        },
    )
    .unwrap()
}

fn primary_runtime(id: &str, display: &str) -> AgentRuntimeDescriptor {
    AgentRuntimeDescriptor::new(
        id,
        display,
        AgentRuntimeKind::Delegated,
        RuntimeCapabilities {
            models: CapabilitySupport::Supported,
            resume: CapabilitySupport::Supported,
            fork: CapabilitySupport::Supported,
            steer: CapabilitySupport::Supported,
            permissions: CapabilitySupport::Supported,
            compaction: CapabilitySupport::Supported,
            ..RuntimeCapabilities::unknown()
        },
    )
    .unwrap()
}

fn rows() -> Vec<heycode_tui::route_picker::RoutePickerRow> {
    build_route_rows(
        &[
            provider("api-a", "API Alpha", "model-a"),
            provider("api-b", "API Beta", "model-b"),
        ],
        &[
            runtime("native", "heycode native", AgentRuntimeKind::Native),
            runtime("claude", "Claude subscription", AgentRuntimeKind::Delegated),
            primary_runtime("codex", "Codex subscription"),
        ],
        "api-a",
        "native",
    )
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 26)).unwrap();
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
fn rows_keep_inference_native_and_delegated_classes_distinct_and_truthful() {
    let rows = rows();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].class, RoutePickerClass::NativeAgent);
    assert_eq!(rows[1].class, RoutePickerClass::DelegatedAgent);
    assert_eq!(rows[2].class, RoutePickerClass::DelegatedAgent);
    assert_eq!(rows[3].class, RoutePickerClass::InferenceApi);
    assert!(rows[0].current);
    assert!(rows[0].selection.is_some());
    assert!(rows[1].selection.is_none());
    assert!(
        rows[1]
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("primary runtime bridge")
    );
    assert!(matches!(
        rows[2].selection,
        Some(RoutePickerSelection::DelegatedRuntime { ref runtime }) if runtime == "codex"
    ));
    assert!(rows[3].current);
    assert!(matches!(
        rows[3].selection,
        Some(RoutePickerSelection::Inference {
            ref provider,
            ref default_model,
        }) if provider == "api-a" && default_model == "model-a"
    ));

    assert_eq!(
        filter_routes(&rows, RoutePickerFilter::AgentRuntimes, "").len(),
        3
    );
    assert_eq!(
        filter_routes(&rows, RoutePickerFilter::InferenceApis, "beta")[0]
            .row
            .id,
        "api-b"
    );
    let subscription_ids = filter_routes(&rows, RoutePickerFilter::All, "subscription")
        .into_iter()
        .map(|matched| matched.row.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        subscription_ids,
        ["claude".to_owned(), "codex".to_owned()]
            .into_iter()
            .collect()
    );
}

#[test]
fn picker_renders_classes_blocks_delegated_and_returns_only_activatable_selection() {
    let mut state = AppState::new("model-a", "/workspace".into());
    state.set_welcome(WelcomeStatusView::new(
        "native",
        "api-a",
        "model-a",
        "ask",
        "/workspace".into(),
    ));
    state.open_route_picker("api-a", "native");
    state.apply_route_catalog(rows());
    let frame = frame_text(&mut state);
    assert!(frame.contains("Provider & runtime"), "{frame}");
    assert!(frame.contains("INFERENCE API"), "{frame}");
    assert!(frame.contains("NATIVE AGENT"), "{frame}");
    assert!(frame.contains("DELEGATED AGENT"), "{frame}");
    assert!(frame.contains("primary runtime bridge"), "{frame}");
    assert!(frame.contains("inference api-a"), "{frame}");
    assert!(frame.contains("default model model-a"), "{frame}");
    assert!(!frame.contains("Welcome to heycode"), "{frame}");
    assert!(frame.contains("Provider & runtime ─"), "{frame}");
    assert!(!frame.contains("╭ Provider & runtime"), "{frame}");

    for character in "claude".chars() {
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Char(character)));
    }
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.route_picker().is_some());
    assert!(state.take_route_selection().is_none());

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    state.open_route_picker("api-a", "native");
    state.apply_route_catalog(rows());
    for character in "codex".chars() {
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Char(character)));
    }
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.take_route_selection(),
        Some(RoutePickerSelection::DelegatedRuntime {
            runtime: "codex".to_owned(),
        })
    );

    state.open_route_picker("api-a", "native");
    state.apply_route_catalog(rows());
    for character in "api-b".chars() {
        state.handle_terminal_event(&key(crossterm::event::KeyCode::Char(character)));
    }
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.route_picker().is_none());
    assert_eq!(
        state.take_route_selection(),
        Some(RoutePickerSelection::Inference {
            provider: "api-b".to_owned(),
            default_model: "model-b".to_owned(),
        })
    );
}

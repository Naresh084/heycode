//! U07 live catalog filtering, stale state, keyboard and frame contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::BackendControlOwner;
use heycode_llm::{
    CapabilitySupport, CatalogError, CatalogFailureKind, CatalogFreshness, CatalogRefreshMode,
    CatalogSnapshot, CatalogView, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ProviderDescriptor, ProviderProtocol,
};
use heycode_tui::app::{AppState, ModelPickerSelection};
use heycode_tui::model_picker::{ModelPickerFilter, filter_models};
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

fn capabilities(tools: CapabilitySupport, reasoning: CapabilitySupport) -> ModelCapabilities {
    ModelCapabilities {
        tools,
        reasoning,
        ..ModelCapabilities::unknown()
    }
}

fn snapshot() -> Arc<CatalogSnapshot> {
    Arc::new(CatalogSnapshot {
        provider: ProviderDescriptor {
            id: "deepseek".to_owned(),
            display_name: "DeepSeek".to_owned(),
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        },
        models: vec![
            ModelDescriptor {
                pricing: heycode_llm::ModelPricing::unknown(),
                performance: heycode_llm::ModelPerformance::unknown(),

                id: "deepseek-stable".to_owned(),
                display_name: "Stable Coder".to_owned(),
                aliases: vec!["coder".to_owned()],
                created_at_ms: None,
                context_window: Some(128_000),
                max_output_tokens: Some(8_192),
                lifecycle: ModelLifecycle::stable(),
                capabilities: capabilities(
                    CapabilitySupport::Supported,
                    CapabilitySupport::Unknown,
                ),
                reasoning: None,
            },
            ModelDescriptor {
                pricing: heycode_llm::ModelPricing::unknown(),
                performance: heycode_llm::ModelPerformance::unknown(),

                id: "deepseek-preview".to_owned(),
                display_name: "Preview Reasoner".to_owned(),
                aliases: vec!["reasoner".to_owned()],
                created_at_ms: None,
                context_window: Some(256_000),
                max_output_tokens: Some(16_384),
                lifecycle: ModelLifecycle::preview(),
                capabilities: capabilities(
                    CapabilitySupport::Unknown,
                    CapabilitySupport::Supported,
                ),
                reasoning: None,
            },
            ModelDescriptor {
                pricing: heycode_llm::ModelPricing::unknown(),
                performance: heycode_llm::ModelPerformance::unknown(),

                id: "deepseek-retired".to_owned(),
                display_name: "Retired".to_owned(),
                aliases: Vec::new(),
                created_at_ms: None,
                context_window: None,
                max_output_tokens: None,
                lifecycle: ModelLifecycle::retired(None, vec!["deepseek-stable".to_owned()]),
                capabilities: capabilities(
                    CapabilitySupport::Supported,
                    CapabilitySupport::Supported,
                ),
                reasoning: None,
            },
        ],
        revision: 4,
        fetched_at_ms: 1,
    })
}

fn key(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
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
fn filters_are_retirement_and_unknown_safe_and_searches_aliases() {
    let snapshot = snapshot();
    assert_eq!(
        filter_models(&snapshot, ModelPickerFilter::Selectable, "", 10)
            .iter()
            .map(|row| row.model.id.as_str())
            .collect::<Vec<_>>(),
        ["deepseek-stable", "deepseek-preview"]
    );
    assert_eq!(
        filter_models(&snapshot, ModelPickerFilter::Stable, "", 10)[0]
            .model
            .id,
        "deepseek-stable"
    );
    assert_eq!(
        filter_models(&snapshot, ModelPickerFilter::Tools, "", 10)[0]
            .model
            .id,
        "deepseek-stable",
        "unknown tool support must not satisfy Tools"
    );
    assert_eq!(
        filter_models(&snapshot, ModelPickerFilter::Reasoning, "", 10)[0]
            .model
            .id,
        "deepseek-preview"
    );
    assert_eq!(
        filter_models(&snapshot, ModelPickerFilter::Selectable, "reasoner", 10)[0]
            .model
            .id,
        "deepseek-preview"
    );
}

#[test]
fn catalog_assertions_are_visible_but_never_silently_become_provider_support() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog-overrides.toml");
    std::fs::write(
        &path,
        r#"schema_version = 1
[[model]]
provider = "deepseek"
model = "deepseek-preview"
[model.capabilities]
tools = "supported"

[[model]]
provider = "deepseek"
model = "deepseek-stable"
[model.capabilities]
tools = "supported"

[[model]]
provider = "deepseek"
model = "missing-model"
[model.capabilities]
tools = "supported"
"#,
    )
    .unwrap();
    let overrides = heycode_catalog_file::CatalogOverrides::load(
        &heycode_catalog_file::CatalogOverridesConfig::new(vec![
            heycode_catalog_file::CatalogOverrideLayer::new("user", path),
        ]),
    )
    .unwrap();
    let mut state = AppState::new("deepseek-stable", "/workspace".into());
    state.set_catalog_overrides(Some(Arc::new(overrides)));
    state.open_model_picker(
        BackendControlOwner::NativeInference {
            provider: "deepseek".to_owned(),
        },
        1,
        "deepseek-stable",
    );
    let mut provider_snapshot = (*snapshot()).clone();
    provider_snapshot.models[0].capabilities.tools = CapabilitySupport::Unsupported;
    state.apply_model_catalog(CatalogView {
        snapshot: Arc::new(provider_snapshot),
        freshness: CatalogFreshness::Live,
        warning: None,
    });

    let picker = state.model_picker.as_ref().unwrap();
    let preview = picker
        .matches()
        .iter()
        .find(|row| row.model.id == "deepseek-preview")
        .unwrap();
    assert_eq!(preview.model.capabilities.tools, CapabilitySupport::Unknown);
    assert_eq!(preview.assertions.len(), 1);
    let stable = picker
        .matches()
        .iter()
        .find(|row| row.model.id == "deepseek-stable")
        .unwrap();
    assert!(stable.has_contradiction);
    assert_eq!(picker.unmatched_overrides().len(), 1);
    let frame = frame_text(&mut state);
    assert!(frame.contains("override:"), "{frame}");
    assert!(frame.contains("unmatched override"), "{frame}");
    assert!(frame.contains("CONFLICT"), "{frame}");

    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        state.model_picker.as_ref().unwrap().filter(),
        ModelPickerFilter::Tools
    );
    assert!(
        state
            .model_picker
            .as_ref()
            .unwrap()
            .matches()
            .iter()
            .all(|row| row.model.id != "deepseek-preview"),
        "a visible user assertion must not masquerade as provider-native support"
    );
}

#[test]
fn stale_catalog_renders_badges_filters_refreshes_and_returns_selection() {
    let mut state = AppState::new("deepseek-stable", "/workspace".into());
    state.apply(&heycode_agent::UiEvent::ModelPickerRequested {
        owner: BackendControlOwner::NativeInference {
            provider: "deepseek".to_owned(),
        },
        routing_revision: 7,
        current_model: "deepseek-stable".to_owned(),
    });
    assert_eq!(
        state.take_model_refresh_request(),
        Some((
            BackendControlOwner::NativeInference {
                provider: "deepseek".to_owned(),
            },
            CatalogRefreshMode::PreferCache
        ))
    );
    state.apply_model_catalog(CatalogView {
        snapshot: snapshot(),
        freshness: CatalogFreshness::StaleFallback,
        warning: Some(CatalogError::Refresh {
            provider: "deepseek".to_owned(),
            kind: CatalogFailureKind::Network,
            message: "safe refresh warning".to_owned(),
        }),
    });
    let frame = frame_text(&mut state);
    for expected in [
        "Select model",
        "stale fallback",
        "safe refresh warning",
        "deepseek-stable",
        "current",
        "stable",
        "tools✓",
        "reason?",
    ] {
        assert!(frame.contains(expected), "missing {expected:?}: {frame}");
    }

    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        state.model_picker.as_ref().unwrap().filter(),
        ModelPickerFilter::Stable
    );
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        state.model_picker.as_ref().unwrap().filter(),
        ModelPickerFilter::Tools
    );
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Char('r'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert_eq!(
        state.take_model_refresh_request(),
        Some((
            BackendControlOwner::NativeInference {
                provider: "deepseek".to_owned(),
            },
            CatalogRefreshMode::Force
        ))
    );

    state.apply_model_catalog(CatalogView {
        snapshot: snapshot(),
        freshness: CatalogFreshness::Live,
        warning: None,
    });
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        state.take_model_selection(),
        Some(ModelPickerSelection {
            owner: BackendControlOwner::NativeInference {
                provider: "deepseek".to_owned(),
            },
            revision: 7,
            catalog: snapshot(),
            model: "deepseek-stable".to_owned(),
            scope: heycode_routing::SelectionScope::Default,
            effort: None,
        })
    );
    assert!(state.model_picker.is_none());
    assert!(state.take_model_refresh_cancel());
}

#[test]
fn error_and_escape_are_visible_and_cancel_the_wait() {
    let mut state = AppState::new("m", "/workspace".into());
    state.open_model_picker(
        BackendControlOwner::NativeInference {
            provider: "missing".to_owned(),
        },
        1,
        "m",
    );
    state.take_model_refresh_request();
    state.apply_model_catalog_error("no model catalog is registered");
    let frame = frame_text(&mut state);
    assert!(frame.contains("no model catalog is registered"), "{frame}");
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(state.model_picker.is_none());
    assert!(state.take_model_refresh_cancel());
}

#[test]
fn delegated_model_and_effort_requests_retain_their_backend_owner() {
    let mut state = AppState::new("native-fallback", "/workspace".into());
    let delegated = BackendControlOwner::DelegatedRuntime {
        runtime: "claude".to_owned(),
    };
    state.apply(&heycode_agent::UiEvent::ModelPickerRequested {
        owner: delegated.clone(),
        routing_revision: 17,
        current_model: "claude-sonnet".to_owned(),
    });
    assert_eq!(
        state.take_model_refresh_request(),
        Some((delegated.clone(), CatalogRefreshMode::PreferCache))
    );
    assert_eq!(
        state.model_picker.as_ref().map(|picker| picker.owner()),
        Some(&delegated)
    );

    state.apply(&heycode_agent::UiEvent::EffortPickerRequested {
        owner: delegated.clone(),
        routing_revision: 17,
        current_effort: Some("medium".to_owned()),
        choices: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        default_effort: Some("medium".to_owned()),
    });
    assert!(state.model_picker.is_none());
    assert!(state.take_model_refresh_cancel());
    let Some(picker) = state.effort_picker.as_ref() else {
        panic!("effort picker should be open");
    };
    assert_eq!(picker.owner(), &delegated);
    assert_eq!(picker.current_effort(), Some("medium"));
    assert_eq!(picker.default_effort(), Some("medium"));
    assert_eq!(picker.choices(), ["low", "medium", "high"]);
    let frame = frame_text(&mut state);
    for expected in [
        "Effort",
        "claude · native-fallback",
        "medium",
        "current · default",
    ] {
        assert!(frame.contains(expected), "missing {expected:?}: {frame}");
    }
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Right,
        crossterm::event::KeyModifiers::NONE,
    ));
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        state.take_effort_selection(),
        Some((
            delegated.clone(),
            17,
            "high".to_owned(),
            heycode_routing::SelectionScope::Default
        ))
    );
    state.open_effort_picker(
        delegated.clone(),
        18,
        Some("high".into()),
        vec!["low".into(), "high".into()],
        Some("high".into()),
    );
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Left,
        crossterm::event::KeyModifiers::NONE,
    ));
    state.handle_terminal_event(&key(
        crossterm::event::KeyCode::Char('s'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        state.take_effort_selection(),
        Some((
            delegated,
            18,
            "low".into(),
            heycode_routing::SelectionScope::Session
        ))
    );
    assert!(state.effort_picker.is_none());
}

#[test]
fn effort_pointer_and_paste_preserve_selection_and_composer() {
    use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut state = AppState::new("native", "/workspace".into());
    state.input.insert_str("preserved draft");
    state.apply(&heycode_agent::UiEvent::EffortPickerRequested {
        owner: BackendControlOwner::NativeInference {
            provider: "native".to_owned(),
        },
        routing_revision: 9,
        current_effort: Some("medium".to_owned()),
        choices: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        default_effort: Some("medium".to_owned()),
    });
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let rows: Vec<String> = terminal
        .backend()
        .buffer()
        .content
        .chunks(100)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect();
    let row = rows
        .iter()
        .position(|row| row.contains("low") && row.contains("medium") && row.contains("high"))
        .unwrap();
    let col = rows[row].find("high").unwrap();
    state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: u16::try_from(col).unwrap(),
        row: u16::try_from(row).unwrap(),
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(state.effort_picker.as_ref().unwrap().selected(), 1);
    assert!(state.take_effort_selection().is_none());
    state.handle_terminal_event(&Event::Paste("/quit\n".to_owned()));
    assert_eq!(state.input.lines(), ["preserved draft"]);
    state.handle_terminal_event(&key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(state.effort_picker.is_none());
    assert!(state.take_effort_selection().is_none());
    assert_eq!(state.input.lines(), ["preserved draft"]);
}

#[test]
fn model_session_shortcut_does_not_consume_search_letters() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut state = AppState::new("deepseek-stable", "/workspace".into());
    let owner = BackendControlOwner::NativeInference {
        provider: "deepseek".into(),
    };
    state.open_model_picker(owner.clone(), 4, "deepseek-stable");
    state.apply_model_catalog(CatalogView {
        snapshot: snapshot(),
        freshness: CatalogFreshness::Live,
        warning: None,
    });
    state.handle_terminal_event(&key(KeyCode::Char('s'), KeyModifiers::NONE));
    let ModelPickerSelection {
        revision,
        model,
        scope,
        effort,
        ..
    } = state.take_model_selection().unwrap();
    assert_eq!(effort, None);
    assert_eq!(
        (revision, model.as_str(), scope),
        (
            4,
            "deepseek-stable",
            heycode_routing::SelectionScope::Session
        )
    );
    state.open_model_picker(owner, 5, "deepseek-stable");
    state.apply_model_catalog(CatalogView {
        snapshot: snapshot(),
        freshness: CatalogFreshness::Live,
        warning: None,
    });
    for character in "/s".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert_eq!(state.model_picker.as_ref().unwrap().query(), "s");
    assert!(state.take_model_selection().is_none());
    state.handle_terminal_event(&key(KeyCode::Backspace, KeyModifiers::NONE));
    state.handle_terminal_event(&key(KeyCode::Char('s'), KeyModifiers::NONE));
    assert_eq!(state.model_picker.as_ref().unwrap().query(), "s");
    assert!(state.take_model_selection().is_none());
}

#[test]
fn effort_scale_orders_known_levels_without_inventing_backend_choices() {
    let mut state = AppState::new("native", "/workspace".into());
    let owner = BackendControlOwner::NativeInference {
        provider: "native".into(),
    };
    state.open_effort_picker(
        owner.clone(),
        1,
        Some("high".into()),
        vec!["max".into(), "high".into(), "low".into()],
        Some("max".into()),
    );
    let picker = state.effort_picker.as_ref().unwrap();
    assert_eq!(picker.choices(), ["low", "high", "max"]);
    assert_eq!(picker.selected(), 1);
    state.open_effort_picker(
        owner,
        2,
        Some("auto".into()),
        vec!["auto".into(), "enabled".into(), "disabled".into()],
        None,
    );
    assert_eq!(
        state.effort_picker.as_ref().unwrap().choices(),
        ["auto", "enabled", "disabled"]
    );
}

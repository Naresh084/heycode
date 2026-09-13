//! Integrated wizard frame and keyboard routing contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_onboarding::{OnboardingService, OnboardingStep};
use heycode_tui::app::{AppState, authorization_options, authorization_options_for_provider};
use ratatui::{Terminal, backend::TestBackend};

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(84, 24)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
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
fn no_runtime_boot_offers_all_families_and_selects_with_one_enter() {
    let onboarding = Arc::new(OnboardingService::new(true));
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    let welcome = frame(&mut state);
    for label in [
        "Welcome to heycode",
        "Use a subscription",
        "Select a provider",
        "Use a local model",
    ] {
        assert!(welcome.contains(label), "{welcome}");
    }
    assert!(
        !welcome.contains("Ask anything"),
        "composer must be blocked"
    );
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.onboarding_outcome,
        Some(heycode_onboarding::OnboardingOutcome::RuntimeClassSelected(
            heycode_onboarding::RuntimeClass::Subscription
        ))
    );
}

#[test]
fn onboarding_preserves_double_ctrl_c_quit_contract() {
    let onboarding = Arc::new(OnboardingService::new(true));
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    let ctrl_c = crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code: crossterm::event::KeyCode::Char('c'),
        modifiers: crossterm::event::KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });
    assert!(!state.handle_terminal_event(&ctrl_c));
    assert!(state.handle_terminal_event(&ctrl_c));
}

#[test]
fn connect_event_reopens_an_inactive_plugin_owned_wizard() {
    let onboarding = Arc::new(OnboardingService::new(false));
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding.clone());
    assert!(!state.onboarding.as_ref().unwrap().active);
    onboarding.begin_connect().unwrap();
    state.apply(&heycode_agent::UiEvent::ConnectRequested);
    assert!(state.onboarding.as_ref().unwrap().active);
    assert_eq!(
        state.onboarding.as_ref().unwrap().step,
        OnboardingStep::RuntimeClass
    );
}

#[test]
fn completed_onboarding_requests_recomposition_without_quitting() {
    let onboarding = Arc::new(OnboardingService::new(true));
    onboarding.complete().unwrap();
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    assert!(frame(&mut state).contains("Continue to heycode"));

    assert!(!state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter)));
    assert!(!state.quit_requested);
    assert_eq!(
        state.onboarding_outcome,
        Some(heycode_onboarding::OnboardingOutcome::ReadyToRecompose)
    );
}

#[test]
fn authorization_pages_filter_connector_methods_and_render_dynamic_rows() {
    use heycode_authorization::{
        AuthorizationDescriptor, AuthorizationFlowId, AuthorizationMethod,
    };
    use heycode_onboarding::{OnboardingOption, RuntimeClass};

    let descriptor = |id: &str, method| AuthorizationDescriptor {
        id: AuthorizationFlowId::new(id).unwrap(),
        label: id.to_owned(),
        method,
        interactive: true,
        query: heycode_credentials::CredentialQuery::new(
            heycode_credentials::CredentialReference::new("TEST_API_KEY").unwrap(),
            heycode_credentials::CredentialKind::new("api-key").unwrap(),
        ),
    };
    let descriptors = vec![
        descriptor("api-key", AuthorizationMethod::ApiKey),
        descriptor("oauth", AuthorizationMethod::OAuth),
        descriptor("device", AuthorizationMethod::DeviceCode),
        descriptor("command", AuthorizationMethod::Command),
        descriptor("ambient", AuthorizationMethod::Ambient),
    ];
    assert_eq!(
        authorization_options(RuntimeClass::ApiOrRouter, descriptors.clone()).len(),
        2
    );
    assert_eq!(
        authorization_options(RuntimeClass::Subscription, descriptors.clone()).len(),
        3
    );
    assert_eq!(
        authorization_options(RuntimeClass::Cloud, descriptors.clone()).len(),
        2
    );
    assert_eq!(
        authorization_options(RuntimeClass::Local, descriptors).len(),
        1
    );

    let provider_options = authorization_options_for_provider(
        RuntimeClass::ApiOrRouter,
        vec![
            descriptor("deepseek-api-key", AuthorizationMethod::ApiKey),
            descriptor("openrouter-api-key", AuthorizationMethod::ApiKey),
        ],
        "openrouter",
    );
    assert_eq!(provider_options.len(), 1);
    assert_eq!(provider_options[0].id, "openrouter-api-key");

    let onboarding = Arc::new(OnboardingService::new(true));
    onboarding
        .show_authorization_methods(vec![OnboardingOption {
            id: "openrouter-api-key".to_owned(),
            label: "OpenRouter API key".to_owned(),
            description: "ApiKey".to_owned(),
        }])
        .unwrap();
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    let text = frame(&mut state);
    assert!(text.contains("Connect your account"), "{text}");
    assert!(text.contains("OpenRouter API key"), "{text}");
}

#[test]
fn api_connections_use_owned_credential_bindings_instead_of_flow_name_conventions() {
    use heycode_authorization::{
        AuthorizationDescriptor, AuthorizationFlowId, AuthorizationMethod,
    };
    let profile = |id: &str, reference: &str| heycode_llm::ProviderProfile {
        registry_name: id.into(),
        descriptor: heycode_llm::ProviderDescriptor {
            id: id.into(),
            display_name: format!("Provider {id}"),
            protocols: Vec::new(),
        },
        default_model: "default".into(),
        credential_reference: Some(reference.into()),
    };
    let flow = |id: &str, reference: &str| AuthorizationDescriptor {
        id: AuthorizationFlowId::new(id).unwrap(),
        label: "Sign in".into(),
        method: AuthorizationMethod::ApiKey,
        interactive: true,
        query: heycode_credentials::CredentialQuery::new(
            heycode_credentials::CredentialReference::new(reference).unwrap(),
            heycode_credentials::CredentialKind::new("api-key").unwrap(),
        ),
    };
    let options = heycode_tui::app::api_connection_options(
        vec![
            flow("custom-login", "BETA_KEY"),
            flow("alpha-api-key", "WRONG_KEY"),
            flow("first-login", "ALPHA_KEY"),
        ],
        &[
            profile("alpha", "ALPHA_KEY").into(),
            profile("beta", "BETA_KEY").into(),
        ],
    );
    assert_eq!(
        options
            .iter()
            .map(|row| (row.id.as_str(), row.label.as_str()))
            .collect::<Vec<_>>(),
        [
            ("custom-login", "Provider beta"),
            ("first-login", "Provider alpha")
        ]
    );
}

#[test]
fn welcome_distinguishes_option_titles_from_descriptions_and_keeps_footer_visible() {
    use ratatui::style::Modifier;
    let onboarding = Arc::new(OnboardingService::new(true));
    let mut state = AppState::new("not connected", "/workspace".into());
    state.set_onboarding(onboarding);
    let mut terminal = Terminal::new(TestBackend::new(84, 24)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, &mut state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let locate = |text: &str| {
        (0..24)
            .find_map(|y| {
                let line: String = (0..84).map(|x| buffer[(x, y)].symbol()).collect();
                line.find(text)
                    .map(|x| (line[..x].chars().count() as u16, y))
            })
            .unwrap_or_else(|| panic!("missing {text}"))
    };
    let title = locate("Select a provider");
    let description = locate("Connect to your API provider");
    assert!(buffer[title].modifier.contains(Modifier::BOLD));
    assert!(!buffer[description].modifier.contains(Modifier::BOLD));
    assert_ne!(buffer[title].fg, buffer[description].fg);
    locate("Enter select");
    locate("Esc quit");
    let selected = locate("Use a subscription");
    assert!(buffer[selected].modifier.contains(Modifier::REVERSED));
    assert!(!buffer[title].modifier.contains(Modifier::REVERSED));
}

#[test]
fn cloud_coordinate_form_renders_provider_metadata_and_routes_terminal_input() {
    let onboarding = Arc::new(OnboardingService::new(true));
    onboarding
        .show_parameters(
            "bedrock",
            heycode_onboarding::OnboardingParameterCredential::Masked,
            vec![heycode_onboarding::OnboardingParameter {
                id: "region".into(),
                label: "AWS region".into(),
                description: "Region used for Bedrock model discovery and inference".into(),
                value: "us-east-1".into(),
            }],
        )
        .unwrap();
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    let rendered = frame(&mut state);
    for text in [
        "Configure the cloud connection",
        "Region used for Bedrock model discovery and inference",
        "AWS region (1/1): us-east-1",
        "Find models",
        "Use a credential",
    ] {
        assert!(rendered.contains(text), "missing {text}: {rendered}");
    }

    let clear = crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code: crossterm::event::KeyCode::Char('u'),
        modifiers: crossterm::event::KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });
    state.handle_terminal_event(&clear);
    state.handle_terminal_event(&crossterm::event::Event::Paste("ap-southeast-2".into()));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.onboarding_outcome,
        Some(heycode_onboarding::OnboardingOutcome::ParametersSelected {
            provider: "bedrock".into(),
            parameters: std::collections::BTreeMap::from([(
                "region".into(),
                "ap-southeast-2".into(),
            )]),
            authenticate: true,
        })
    );
}

#[test]
fn vertex_coordinate_form_never_offers_masked_credential_entry() {
    let onboarding = Arc::new(OnboardingService::new(true));
    onboarding
        .show_parameters(
            "vertex-google",
            heycode_onboarding::OnboardingParameterCredential::Unavailable,
            vec![
                heycode_onboarding::OnboardingParameter {
                    id: "project".into(),
                    label: "Google Cloud project".into(),
                    description: "Project id or number".into(),
                    value: "vertex-fixture".into(),
                },
                heycode_onboarding::OnboardingParameter {
                    id: "location".into(),
                    label: "Vertex AI location".into(),
                    description: "Region or global".into(),
                    value: "us-central1".into(),
                },
            ],
        )
        .unwrap();
    onboarding
        .apply(heycode_onboarding::OnboardingAction::Confirm)
        .unwrap();
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    let rendered = frame(&mut state);
    assert!(rendered.contains("Vertex AI location (2/2): us-central1"));
    assert!(rendered.contains("Find models"));
    assert!(!rendered.contains("Use a credential"));
}

#[test]
fn onboarding_search_consumes_typing_and_paste_and_renders_empty_results() {
    let onboarding = Arc::new(OnboardingService::new(true));
    onboarding
        .show_authorization_methods(
            ["Fireworks", "Groq", "Google"]
                .into_iter()
                .map(|name| heycode_onboarding::OnboardingOption {
                    id: name.into(),
                    label: name.into(),
                    description: String::new(),
                })
                .collect(),
        )
        .unwrap();
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    state.handle_terminal_event(&crossterm::event::Event::Paste("gro".into()));
    assert_eq!(state.onboarding.as_ref().unwrap().options[0].id, "Groq");
    assert!(frame(&mut state).contains("Search: gro"));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Char('x')));
    assert!(frame(&mut state).contains("No matches"));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.onboarding_outcome.is_none());
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Backspace));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.onboarding_outcome,
        Some(heycode_onboarding::OnboardingOutcome::AuthorizationFlowSelected("Groq".into()))
    );
}

#[test]
fn explicit_custom_model_id_is_visible_and_routes_through_terminal_input() {
    let onboarding = Arc::new(OnboardingService::new(true));
    onboarding.show_models_with_explicit(Vec::new()).unwrap();
    let mut state = AppState::new("not connected", std::path::PathBuf::from("/workspace"));
    state.set_onboarding(onboarding);
    assert!(frame(&mut state).contains("type an exact model ID"));
    state.handle_terminal_event(&crossterm::event::Event::Paste("local/model:latest".into()));
    let rendered = frame(&mut state);
    assert!(rendered.contains("Use model ID local/model:latest"));
    assert!(rendered.contains("not discovered"));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        state.onboarding_outcome,
        Some(heycode_onboarding::OnboardingOutcome::ModelSelected(
            "local/model:latest".into()
        ))
    );
}

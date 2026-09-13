//! Deterministic onboarding state and outcome contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::compose;
use heycode_onboarding::{
    OnboardingAction, OnboardingOutcome, OnboardingParameter, OnboardingParameterCredential,
    OnboardingService, OnboardingStep, RuntimeClass, SERVICE_ONBOARDING, onboarding_plugin,
};

#[test]
fn welcome_offers_connection_families_directly_and_selects_in_one_step() {
    let service = OnboardingService::new(true);
    let welcome = service.snapshot().unwrap();
    assert!(welcome.active);
    assert_eq!(welcome.step, OnboardingStep::Welcome);
    assert_eq!(welcome.selected, 0);
    assert_eq!(
        welcome
            .options
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["subscription", "local", "provider"]
    );
    let visible = welcome
        .options
        .iter()
        .map(|row| format!("{} {}", row.label, row.description))
        .collect::<Vec<_>>()
        .join(" ");
    for deferred in ["other local", "coming soon", "mock"] {
        assert!(
            !visible.contains(deferred),
            "unfinished capability advertised: {deferred}"
        );
    }
    assert!(!visible.contains("Use a cloud platform"));
    assert!(visible.contains("custom server"));
    service.apply(OnboardingAction::Next).unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::RuntimeClassSelected(RuntimeClass::Local)
    );
}

#[test]
fn optional_wizard_is_inactive_and_exit_is_explicit() {
    let service = OnboardingService::new(false);
    assert!(!service.snapshot().unwrap().active);

    let required = OnboardingService::new(true);
    assert_eq!(
        required.apply(OnboardingAction::Cancel).unwrap(),
        OnboardingOutcome::Exit
    );
}

#[test]
fn every_welcome_choice_has_a_distinct_connection_family() {
    for (index, family) in [
        RuntimeClass::Subscription,
        RuntimeClass::Local,
        RuntimeClass::ApiOrRouter,
    ]
    .into_iter()
    .enumerate()
    {
        let service = OnboardingService::new(true);
        for _ in 0..index {
            service.apply(OnboardingAction::Next).unwrap();
        }
        assert_eq!(
            service.apply(OnboardingAction::Confirm).unwrap(),
            OnboardingOutcome::RuntimeClassSelected(family)
        );
    }
}

#[test]
fn plugin_publishes_the_same_state_machine_service() {
    let plugins = vec![onboarding_plugin(true)];
    let context = compose(&plugins).unwrap();
    let service = context
        .get::<OnboardingService>(SERVICE_ONBOARDING)
        .unwrap();
    assert!(service.snapshot().unwrap().active);
    assert_eq!(context.plugin_descriptors()[0].id, "onboarding");
}

#[test]
fn dynamic_authorization_method_page_returns_contributed_flow_id() {
    let service = OnboardingService::new(true);
    service
        .show_authorization_methods(vec![heycode_onboarding::OnboardingOption {
            id: "openrouter-api-key".to_owned(),
            label: "OpenRouter API key".to_owned(),
            description: "Masked key entry".to_owned(),
        }])
        .unwrap();
    let snapshot = service.snapshot().unwrap();
    assert_eq!(snapshot.step, OnboardingStep::AuthorizationMethod);
    assert_eq!(snapshot.options[0].label, "OpenRouter API key");
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::AuthorizationFlowSelected("openrouter-api-key".to_owned())
    );
}

#[test]
fn unavailable_authorization_class_stays_visible_and_returns_to_runtime_choices() {
    let service = OnboardingService::new(true);
    service.show_authorization_methods(Vec::new()).unwrap();

    let snapshot = service.snapshot().unwrap();
    assert_eq!(snapshot.step, OnboardingStep::AuthorizationMethod);
    assert_eq!(snapshot.options.len(), 1);
    assert_eq!(snapshot.options[0].id, "back");
    assert!(snapshot.options[0].label.contains("No compatible method"));

    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::None
    );
    assert_eq!(
        service.snapshot().unwrap().step,
        OnboardingStep::RuntimeClass
    );
}

#[test]
fn assistant_and_model_pages_preserve_back_navigation() {
    let service = OnboardingService::new(true);
    let row = |id: &str| heycode_onboarding::OnboardingOption {
        id: id.into(),
        label: id.into(),
        description: String::new(),
    };
    service
        .show_assistants(vec![row("codex"), row("claude")])
        .unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::AssistantSelected("codex".into())
    );
    service
        .show_models(vec![row("model-one"), row("model-two")])
        .unwrap();
    service.apply(OnboardingAction::Next).unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::ModelSelected("model-two".into())
    );
    service.apply(OnboardingAction::Cancel).unwrap();
    assert_eq!(service.snapshot().unwrap().step, OnboardingStep::Assistant);
    assert_eq!(service.snapshot().unwrap().options.len(), 2);
    service.apply(OnboardingAction::Cancel).unwrap();
    assert_eq!(service.snapshot().unwrap().step, OnboardingStep::Welcome);
}

#[test]
fn api_model_back_returns_to_its_provider_choices() {
    let service = OnboardingService::new(true);
    let row = |id: &str| heycode_onboarding::OnboardingOption {
        id: id.into(),
        label: id.into(),
        description: String::new(),
    };
    service
        .show_authorization_methods(vec![row("provider-login")])
        .unwrap();
    service.show_models(vec![row("provider-model")]).unwrap();
    service.apply(OnboardingAction::Cancel).unwrap();
    assert_eq!(
        service.snapshot().unwrap().step,
        OnboardingStep::AuthorizationMethod
    );
    assert_eq!(service.snapshot().unwrap().options[0].id, "provider-login");
}

#[test]
fn search_confirms_the_filtered_identity_and_no_match_never_selects_hidden_rows() {
    let service = OnboardingService::new(true);
    let row = |id: &str| heycode_onboarding::OnboardingOption {
        id: id.into(),
        label: id.into(),
        description: String::new(),
    };
    service
        .show_authorization_methods(vec![row("fireworks"), row("groq"), row("google")])
        .unwrap();
    for c in "GRO".chars() {
        service.apply(OnboardingAction::Search(c)).unwrap();
    }
    assert_eq!(service.snapshot().unwrap().options.len(), 1);
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::AuthorizationFlowSelected("groq".into())
    );
    service.apply(OnboardingAction::Search('x')).unwrap();
    assert!(service.snapshot().unwrap().options.is_empty());
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::None
    );
    service.apply(OnboardingAction::Backspace).unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::AuthorizationFlowSelected("groq".into())
    );
    service
        .show_models(vec![row("model-a"), row("model-b")])
        .unwrap();
    assert_eq!(
        service.snapshot().unwrap().options.len(),
        2,
        "search resets at page boundaries"
    );
}

#[test]
fn saved_connection_repair_keeps_its_identity_and_never_shows_first_run_welcome() {
    let service = OnboardingService::new(true);
    service
        .begin_reconnect(heycode_onboarding::OnboardingOption {
            id: "openrouter".into(),
            label: "Reconnect OpenRouter".into(),
            description: "Replace the saved credential".into(),
        })
        .unwrap();
    assert_eq!(service.snapshot().unwrap().step, OnboardingStep::Reconnect);
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::ConnectionSelected("openrouter".into())
    );
}

#[test]
fn repair_opened_from_connect_can_return_to_the_existing_session() {
    let service = OnboardingService::new(false);
    service.begin_connect().unwrap();
    service
        .begin_reconnect(heycode_onboarding::OnboardingOption {
            id: "openrouter".into(),
            label: "Reconnect OpenRouter".into(),
            description: "Rejected key".into(),
        })
        .unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Cancel).unwrap(),
        OnboardingOutcome::Dismissed
    );
}

#[test]
fn endpoint_input_is_not_search_and_survives_back_from_models() {
    let service = OnboardingService::new(true);
    service
        .show_endpoint("lmstudio", "http://localhost:1234")
        .unwrap();
    service.apply(OnboardingAction::ClearInput).unwrap();
    for c in "http://localhost:2234".chars() {
        service.apply(OnboardingAction::Search(c)).unwrap();
    }
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::EndpointSelected {
            provider: "lmstudio".into(),
            endpoint: "http://localhost:2234".into(),
            authenticate: false,
        }
    );
    service.show_models(vec![]).unwrap();
    service.apply(OnboardingAction::Cancel).unwrap();
    assert_eq!(
        service.snapshot().unwrap().search.as_deref(),
        Some("http://localhost:2234")
    );
    service.apply(OnboardingAction::Cancel).unwrap();
    assert_eq!(service.snapshot().unwrap().step, OnboardingStep::Connection);
}

#[test]
fn endpoint_key_choice_keeps_the_address_and_requests_masked_authorization() {
    let service = OnboardingService::new(true);
    service
        .show_endpoint("lmstudio", "http://localhost:2234")
        .unwrap();
    service.apply(OnboardingAction::Next).unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::EndpointSelected {
            provider: "lmstudio".into(),
            endpoint: "http://localhost:2234".into(),
            authenticate: true
        }
    );
}

#[test]
fn explicit_model_mode_uses_a_safe_exact_id_when_discovery_has_no_match() {
    let service = OnboardingService::new(true);
    service
        .show_endpoint("custom-openai", "http://localhost:8000/v1")
        .unwrap();
    service.show_models_with_explicit(Vec::new()).unwrap();
    assert!(service.snapshot().unwrap().body.contains("exact model ID"));

    for character in "local/model:latest".chars() {
        service.apply(OnboardingAction::Search(character)).unwrap();
    }
    let snapshot = service.snapshot().unwrap();
    assert_eq!(snapshot.options.len(), 1);
    assert_eq!(snapshot.options[0].id, "local/model:latest");
    assert!(snapshot.options[0].description.contains("not discovered"));
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::ModelSelected("local/model:latest".into())
    );
}

#[test]
fn explicit_model_mode_does_not_replace_a_matching_discovered_row() {
    let service = OnboardingService::new(true);
    service
        .show_models_with_explicit(vec![heycode_onboarding::OnboardingOption {
            id: "local-model".into(),
            label: "Discovered model".into(),
            description: "catalog row".into(),
        }])
        .unwrap();
    for character in "local-model".chars() {
        service.apply(OnboardingAction::Search(character)).unwrap();
    }
    let snapshot = service.snapshot().unwrap();
    assert_eq!(snapshot.options.len(), 1);
    assert_eq!(snapshot.options[0].label, "Discovered model");
}

#[test]
fn cloud_parameters_are_entered_in_order_and_return_one_complete_draft() {
    let service = OnboardingService::new(true);
    service
        .show_parameters(
            "vertex-google",
            OnboardingParameterCredential::Unavailable,
            vec![
                OnboardingParameter {
                    id: "project".into(),
                    label: "Google Cloud project".into(),
                    description: "Project id, not its display name".into(),
                    value: "saved-project".into(),
                },
                OnboardingParameter {
                    id: "location".into(),
                    label: "Vertex location".into(),
                    description: "Regional Vertex endpoint location".into(),
                    value: String::new(),
                },
            ],
        )
        .unwrap();

    let first = service.snapshot().unwrap();
    assert_eq!(first.step, OnboardingStep::Parameters);
    assert_eq!(first.input.as_ref().unwrap().label, "Google Cloud project");
    assert_eq!(first.input.as_ref().unwrap().value, "saved-project");
    assert_eq!(first.options.len(), 1, "intermediate fields only continue");
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::None
    );

    let second = service.snapshot().unwrap();
    assert_eq!(second.input.as_ref().unwrap().label, "Vertex location");
    for character in "us-central1".chars() {
        service.apply(OnboardingAction::Search(character)).unwrap();
    }
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::ParametersSelected {
            provider: "vertex-google".into(),
            parameters: std::collections::BTreeMap::from([
                ("location".into(), "us-central1".into()),
                ("project".into(), "saved-project".into()),
            ]),
            authenticate: false,
        }
    );

    service.apply(OnboardingAction::Cancel).unwrap();
    assert_eq!(service.snapshot().unwrap().step, OnboardingStep::Parameters);
    assert_eq!(
        service.snapshot().unwrap().input.as_ref().unwrap().label,
        "Google Cloud project",
        "Escape walks backwards through fields before leaving the form"
    );
}

#[test]
fn a_blank_required_cloud_parameter_cannot_advance() {
    let service = OnboardingService::new(true);
    service
        .show_parameters(
            "bedrock",
            OnboardingParameterCredential::Masked,
            vec![OnboardingParameter {
                id: "region".into(),
                label: "AWS region".into(),
                description: "Region used by Bedrock control and runtime planes".into(),
                value: String::new(),
            }],
        )
        .unwrap();
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::None
    );
    assert_eq!(service.snapshot().unwrap().step, OnboardingStep::Parameters);
}

#[test]
fn the_complete_parameter_draft_is_observable_for_stale_result_rejection() {
    let service = OnboardingService::new(true);
    service
        .show_parameters(
            "bedrock",
            OnboardingParameterCredential::Masked,
            vec![OnboardingParameter {
                id: "region".into(),
                label: "AWS region".into(),
                description: String::new(),
                value: "us-east-1".into(),
            }],
        )
        .unwrap();
    assert_eq!(
        service.parameter_draft().unwrap(),
        Some(heycode_onboarding::OnboardingParameterDraft {
            provider: "bedrock".into(),
            parameters: std::collections::BTreeMap::from([("region".into(), "us-east-1".into())]),
        })
    );
    service.apply(OnboardingAction::Backspace).unwrap();
    assert_ne!(
        service.parameter_draft().unwrap().unwrap().parameters,
        std::collections::BTreeMap::from([("region".into(), "us-east-1".into())])
    );
}

#[test]
fn model_search_matches_multiple_words_across_name_and_id_without_changing_order() {
    use heycode_onboarding::OnboardingOption;
    let service = OnboardingService::new(true);
    service
        .show_models(vec![
            OnboardingOption {
                id: "anthropic/claude-sonnet-new".into(),
                label: "Newest Sonnet".into(),
                description: String::new(),
            },
            OnboardingOption {
                id: "anthropic/claude-sonnet-old".into(),
                label: "Older Sonnet".into(),
                description: String::new(),
            },
            OnboardingOption {
                id: "anthropic/claude-opus".into(),
                label: "Opus".into(),
                description: String::new(),
            },
        ])
        .unwrap();
    for c in "  CLAUDE  sonnet ".chars() {
        service.apply(OnboardingAction::Search(c)).unwrap();
    }
    assert_eq!(
        service
            .snapshot()
            .unwrap()
            .options
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["anthropic/claude-sonnet-new", "anthropic/claude-sonnet-old"]
    );
    assert_eq!(
        service.apply(OnboardingAction::Confirm).unwrap(),
        OnboardingOutcome::ModelSelected("anthropic/claude-sonnet-new".into())
    );
}

//! CMD02 routing namespace validation and layering contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use heycode_routing::{RoutingSelection, routing_definition, settings_namespace};
use heycode_settings::{SettingsDocuments, SettingsService};

fn ids(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn base() -> RoutingSelection {
    RoutingSelection::new("native", "alpha", "model-a", None).unwrap()
}

#[test]
fn pending_cloud_connection_restores_explicit_nonsecret_parameters() {
    let mut documents = SettingsDocuments::new();
    documents.set_user(settings_namespace().unwrap(), serde_json::json!({
        "pending_connection": {"provider":"bedrock", "model":"chosen-model", "parameters":{"region":"us-east-1"}}
    })).unwrap();
    let requested = heycode_routing::requested_connection(&documents, &base()).unwrap();
    assert_eq!(requested.provider(), "bedrock");
    assert_eq!(
        requested.parameters().get("region").map(String::as_str),
        Some("us-east-1")
    );
}

#[test]
fn user_logout_latch_cannot_be_masked_by_project_route_or_false_latch() {
    let namespace = settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            namespace.clone(),
            serde_json::json!({
                "setup_required": true,
                "runtime": "codex",
                "provider": "beta",
                "model": "model-b"
            }),
        )
        .unwrap();
    documents
        .set_project(
            namespace,
            serde_json::json!({
                "setup_required": false,
                "runtime": "codex",
                "provider": "beta",
                "model": "model-b"
            }),
        )
        .unwrap();

    assert!(heycode_routing::requires_setup(&documents).unwrap());
    assert!(!heycode_routing::has_persisted_connection(&documents).unwrap());
    assert_eq!(
        heycode_routing::requested_runtime(&documents)
            .unwrap()
            .as_deref(),
        Some("native")
    );
    assert_eq!(
        heycode_routing::requested_connection(&documents, &base()).unwrap(),
        base()
    );
}

#[test]
fn composition_base_resolves_to_one_complete_effective_selection() {
    let service = SettingsService::new(SettingsDocuments::new());
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&base(), ids(&["alpha", "beta"]), ids(&["native"])).unwrap(),
        )
        .unwrap();
    let resolved = snapshot.resolved();
    assert_eq!(resolved["runtime"], "native");
    assert_eq!(resolved["provider"], "alpha");
    assert_eq!(resolved["model"], "model-a");
    assert!(resolved["effort"].is_null());
}

#[test]
fn persisted_provider_and_model_override_base_without_copying_unknown_fields() {
    let namespace = settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            namespace,
            serde_json::json!({
                "runtime": "native",
                "provider": "beta",
                "model": "model-b"
            }),
        )
        .unwrap();
    let service = SettingsService::new(documents);
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&base(), ids(&["alpha", "beta"]), ids(&["native"])).unwrap(),
        )
        .unwrap();
    assert_eq!(snapshot.resolved()["provider"], "beta");
    assert_eq!(snapshot.resolved()["model"], "model-b");
}

#[test]
fn unknown_routes_partial_tuples_malformed_effort_and_unknown_fields_fail_before_publication() {
    for invalid in [
        serde_json::json!({
            "runtime": "native", "provider": "missing", "model": "m"
        }),
        serde_json::json!({"runtime": "", "provider": "alpha", "model": ""}),
        serde_json::json!({
            "runtime": "native", "provider": "alpha", "model": "m", "effort": " bad"
        }),
        serde_json::json!({
            "runtime": "native", "provider": "alpha", "model": "m", "surprise": true
        }),
    ] {
        let namespace = settings_namespace().unwrap();
        let mut documents = SettingsDocuments::new();
        documents.set_user(namespace, invalid).unwrap();
        let service = SettingsService::new(documents);
        let context = heycode_core::Context::new();
        assert!(
            service
                .register(
                    &context,
                    routing_definition(&base(), ids(&["alpha"]), ids(&["native"])).unwrap(),
                )
                .is_err()
        );
    }
}

#[test]
fn native_effort_survives_schema_resolution_and_startup_projection() {
    let selection =
        RoutingSelection::new("native", "alpha", "model-a", Some("high".to_owned())).unwrap();
    let service = SettingsService::new(SettingsDocuments::new());
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&selection, ids(&["alpha"]), ids(&["native"])).unwrap(),
        )
        .unwrap();
    assert_eq!(snapshot.resolved()["effort"], "high");

    let mut documents = SettingsDocuments::new();
    documents
        .set_user(settings_namespace().unwrap(), snapshot.resolved().clone())
        .unwrap();
    let requested = heycode_routing::requested_connection(&documents, &base()).unwrap();
    assert_eq!(requested.effort(), Some("high"));

    for invalid in ["", " padded", "padded ", "bad\nvalue"] {
        assert!(
            RoutingSelection::new("native", "alpha", "model-a", Some(invalid.to_owned())).is_err()
        );
    }
}

#[test]
fn command_line_overrides_beat_persisted_routing_and_name_what_they_replaced() {
    use heycode_routing::{RoutingOverrides, routing_definition_with_overrides};

    let namespace = settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            namespace,
            serde_json::json!({
                "runtime": "native",
                "provider": "beta",
                "model": "model-b"
            }),
        )
        .unwrap();
    let service = SettingsService::new(documents);
    let context = heycode_core::Context::new();
    let overrides = RoutingOverrides::new(None, Some("model-x".to_owned()));
    assert!(!overrides.is_empty());
    let snapshot = service
        .register(
            &context,
            routing_definition_with_overrides(
                &base(),
                &overrides,
                ids(&["alpha", "beta"]),
                ids(&["native"]),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        snapshot.resolved()["provider"],
        "beta",
        "only the flagged field is overridden; the persisted provider still applies"
    );
    assert_eq!(
        snapshot.resolved()["model"],
        "model-x",
        "the flag wins this session"
    );

    assert_eq!(
        overrides.notice(&snapshot),
        Some(
            "command line sets model `model-x` for this session (user settings have `model-b`)"
                .to_owned()
        )
    );
    assert_eq!(
        RoutingOverrides::new(None, Some("model-b".to_owned())).notice(&snapshot),
        None,
        "no notice when the flag agrees with what is persisted"
    );
    assert_eq!(RoutingOverrides::default().notice(&snapshot), None);
    assert!(
        routing_definition_with_overrides(
            &base(),
            &RoutingOverrides::default(),
            ids(&["alpha"]),
            ids(&["native"])
        )
        .is_ok()
    );
}

#[test]
fn delegated_model_is_distinct_from_native_fallback_and_validated() {
    let selection = RoutingSelection::new("codex", "alpha", "model-a", None)
        .unwrap()
        .with_runtime_model("codex-model")
        .unwrap();
    assert_eq!(selection.model(), "model-a");
    assert_eq!(selection.runtime_model(), Some("codex-model"));
    assert!(base().with_runtime_model("codex-model").is_err());
    assert!(selection.clone().with_runtime_model("\ninvalid").is_err());
    let service = SettingsService::new(SettingsDocuments::new());
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&selection, ids(&["alpha"]), ids(&["native", "codex"])).unwrap(),
        )
        .unwrap();
    assert_eq!(snapshot.resolved()["runtime_model"], "codex-model");
}

#[test]
fn delegated_effort_round_trips_independently_and_native_routes_reject_it() {
    let selection = RoutingSelection::new(
        "codex",
        "alpha",
        "native-model",
        Some("native-high".to_owned()),
    )
    .unwrap()
    .with_runtime_model("codex-model")
    .unwrap()
    .with_runtime_effort(Some("xhigh".to_owned()))
    .unwrap();
    let service = SettingsService::new(SettingsDocuments::new());
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&selection, ids(&["alpha"]), ids(&["native", "codex"])).unwrap(),
        )
        .unwrap();
    assert_eq!(snapshot.resolved()["model"], "native-model");
    assert_eq!(snapshot.resolved()["effort"], "native-high");
    assert_eq!(snapshot.resolved()["runtime_model"], "codex-model");
    assert_eq!(snapshot.resolved()["runtime_effort"], "xhigh");

    let mut documents = SettingsDocuments::new();
    documents
        .set_user(settings_namespace().unwrap(), snapshot.resolved().clone())
        .unwrap();
    let restored = heycode_routing::requested_connection(&documents, &base()).unwrap();
    assert_eq!(restored.runtime(), "codex");
    assert_eq!(restored.model(), "native-model");
    assert_eq!(restored.effort(), Some("native-high"));
    assert_eq!(restored.runtime_model(), Some("codex-model"));
    assert_eq!(restored.runtime_effort(), Some("xhigh"));

    assert!(base().with_runtime_effort(Some("high".to_owned())).is_err());
    assert!(
        RoutingSelection::new("codex", "alpha", "native-model", None)
            .unwrap()
            .with_runtime_effort(Some(" padded".to_owned()))
            .is_err()
    );
}

#[test]
fn pending_connection_is_separate_from_live_route_and_controls_next_boot() {
    let mut documents = SettingsDocuments::new();
    documents.set_user(settings_namespace().unwrap(), serde_json::json!({
        "runtime":"codex", "provider":"alpha", "model":"model-a", "runtime_model":"codex-model",
        "pending_connection":{"provider":"beta","model":"model-b"}
    })).unwrap();
    let requested = heycode_routing::requested_connection(&documents, &base()).unwrap();
    assert_eq!(requested.provider(), "beta");
    assert_eq!(requested.model(), "model-b");
    assert_eq!(
        heycode_routing::requested_runtime(&documents)
            .unwrap()
            .as_deref(),
        Some("native")
    );
    let service = SettingsService::new(documents);
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&base(), ids(&["alpha", "beta"]), ids(&["native", "codex"]))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(snapshot.resolved()["runtime"], "codex");
    assert_eq!(snapshot.resolved()["provider"], "alpha");
    assert_eq!(
        snapshot.resolved()["pending_connection"]["provider"],
        "beta"
    );
}

#[test]
fn draft_endpoint_survives_startup_resolution_without_changing_the_live_route() {
    let mut documents = SettingsDocuments::new();
    documents.set_user(settings_namespace().unwrap(), serde_json::json!({
        "runtime":"native", "provider":"alpha", "model":"model-a",
        "pending_connection":{"provider":"beta","model":"model-b","endpoint":"http://localhost:2234"}
    })).unwrap();
    let requested = heycode_routing::requested_connection(&documents, &base()).unwrap();
    assert_eq!(requested.endpoint(), Some("http://localhost:2234"));
    assert_eq!(requested.provider(), "beta");
}

#[test]
fn endpoint_credential_reference_survives_the_same_connection_transaction() {
    let mut documents = SettingsDocuments::new();
    documents.set_user(settings_namespace().unwrap(), serde_json::json!({
        "runtime":"native", "provider":"alpha", "model":"model-a",
        "pending_connection":{"provider":"beta","model":"model-b","endpoint":"http://localhost:2234", "credential_reference":"HEYCODE_LOCAL_TEST"}
    })).unwrap();
    let requested = heycode_routing::requested_connection(&documents, &base()).unwrap();
    assert_eq!(
        requested
            .credential_reference()
            .map(|reference| reference.as_str()),
        Some("HEYCODE_LOCAL_TEST")
    );
    assert_eq!(requested.endpoint(), Some("http://localhost:2234"));
}

#[test]
fn persisted_endpoint_rejects_embedded_credentials_and_non_http_addresses() {
    for endpoint in [
        "https://user:secret@example.test",
        "https://example.test/?token=secret",
        "https://example.test/#secret",
        "file:///tmp/model",
        " http://localhost:1234",
    ] {
        assert!(base().with_endpoint(Some(endpoint.into())).is_err());
    }
}

#[test]
fn cloud_parameters_validate_before_settings_publication() {
    for parameters in [
        serde_json::json!({"region": ""}),
        serde_json::json!({"region": " us-east-1"}),
        serde_json::json!({"project": "bad\nproject"}),
        serde_json::json!({"location": 42}),
        serde_json::json!({"api_key": "must-use-credentials"}),
        serde_json::json!({"deployment": "a".repeat(257)}),
    ] {
        for pending in [true, false] {
            let mut documents = SettingsDocuments::new();
            let value = if pending {
                serde_json::json!({"pending_connection": {"provider":"alpha", "model":"m", "parameters":parameters}})
            } else {
                serde_json::json!({"parameters": parameters})
            };
            documents
                .set_user(settings_namespace().unwrap(), value)
                .unwrap();
            assert!(heycode_routing::requested_connection(&documents, &base()).is_err());
            let service = SettingsService::new(documents);
            let context = heycode_core::Context::new();
            assert!(
                service
                    .register(
                        &context,
                        routing_definition(&base(), ids(&["alpha"]), ids(&["native"])).unwrap()
                    )
                    .is_err()
            );
        }
    }
}

#[test]
fn cloud_coordinates_survive_settings_base_and_restart() {
    let selection = base()
        .with_parameters(std::collections::BTreeMap::from([
            ("project".into(), "my-project".into()),
            ("location".into(), "global".into()),
            ("resource".into(), "my-resource".into()),
        ]))
        .unwrap();
    let service = SettingsService::new(SettingsDocuments::new());
    let context = heycode_core::Context::new();
    let snapshot = service
        .register(
            &context,
            routing_definition(&selection, ids(&["alpha"]), ids(&["native"])).unwrap(),
        )
        .unwrap();
    assert_eq!(
        snapshot.resolved()["parameters"],
        serde_json::json!({
            "project":"my-project",
            "location":"global",
            "resource":"my-resource"
        })
    );
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(settings_namespace().unwrap(), snapshot.resolved().clone())
        .unwrap();
    assert_eq!(
        heycode_routing::requested_connection(&documents, &base()).unwrap(),
        selection
    );
}

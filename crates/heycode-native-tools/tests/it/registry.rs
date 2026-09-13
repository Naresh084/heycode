//! N01 logical implementation registry, routing and lifecycle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{NativeToolImplementationKind, NativeToolRoute};
use heycode_native_tools::{
    NativeToolImplementation, NativeToolPolicy, NativeToolPolicyMode, NativeToolRegistry,
    SERVICE_NATIVE_TOOLS, native_tool_policy_namespace, native_tool_policy_plugin,
    native_tools_plugin,
};

#[test]
fn route_value_is_validated_and_round_trips_durably() {
    let route = NativeToolRoute::new(
        "web_search",
        "openrouter:web_search",
        NativeToolImplementationKind::Provider,
        Some("openrouter".to_owned()),
    )
    .unwrap();
    assert_eq!(route.logical(), "web_search");
    assert_eq!(route.implementation(), "openrouter:web_search");
    assert_eq!(route.provider(), Some("openrouter"));
    let encoded = serde_json::to_vec(&route).unwrap();
    let decoded: NativeToolRoute = serde_json::from_slice(&encoded).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, route);
}

#[test]
fn registry_prefers_matching_provider_then_client_and_disposes() {
    let plugins = vec![native_tools_plugin()];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<NativeToolRegistry>(SERVICE_NATIVE_TOOLS)
        .unwrap();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_search",
                "client:web_search",
                NativeToolImplementationKind::Client,
                None,
                10,
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_search",
                "openrouter:web_search",
                NativeToolImplementationKind::Provider,
                Some("openrouter".to_owned()),
                10,
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_fetch",
                "client:web_fetch",
                NativeToolImplementationKind::Client,
                None,
                10,
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(
        registry
            .resolve("openrouter")
            .unwrap()
            .iter()
            .map(|route| (route.logical(), route.implementation()))
            .collect::<Vec<_>>(),
        [
            ("web_fetch", "client:web_fetch"),
            ("web_search", "openrouter:web_search"),
        ]
    );
    assert_eq!(
        registry
            .resolve("deepseek")
            .unwrap()
            .iter()
            .map(|route| (route.logical(), route.implementation()))
            .collect::<Vec<_>>(),
        [
            ("web_fetch", "client:web_fetch"),
            ("web_search", "client:web_search"),
        ]
    );

    context.shutdown();
    assert!(registry.resolve("openrouter").unwrap().is_empty());
}

#[test]
fn invalid_persisted_policy_fails_plugin_composition() {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            native_tool_policy_namespace().unwrap(),
            serde_json::json!({"default":"sometimes","overrides":{}}),
        )
        .unwrap();
    let plugins = vec![
        heycode_settings::settings_plugin(documents),
        native_tools_plugin(),
        native_tool_policy_plugin(),
    ];
    assert!(heycode_core::compose(&plugins).is_err());
}

#[test]
fn duplicate_implementation_and_invalid_provider_ownership_fail() {
    let context = heycode_core::Context::new();
    let registry = NativeToolRegistry::new();
    let implementation = NativeToolImplementation::new(
        "web_search",
        "client:web_search",
        NativeToolImplementationKind::Client,
        None,
        10,
    )
    .unwrap();
    registry.register(&context, implementation.clone()).unwrap();
    assert!(registry.register(&context, implementation).is_err());
    assert!(
        NativeToolImplementation::new(
            "web_search",
            "provider:missing-owner",
            NativeToolImplementationKind::Provider,
            None,
            10,
        )
        .is_err()
    );
}

#[test]
fn explicit_policy_modes_choose_or_refuse_provider_and_local_candidates() {
    let context = heycode_core::Context::new();
    let registry = NativeToolRegistry::new();
    for implementation in [
        NativeToolImplementation::new(
            "web_search",
            "client:web_search",
            NativeToolImplementationKind::Client,
            None,
            10,
        )
        .unwrap(),
        NativeToolImplementation::new(
            "web_search",
            "mcp:web_search",
            NativeToolImplementationKind::Mcp,
            None,
            50,
        )
        .unwrap(),
        NativeToolImplementation::new(
            "web_search",
            "openrouter:web_search",
            NativeToolImplementationKind::Provider,
            Some("openrouter".to_owned()),
            1,
        )
        .unwrap(),
        NativeToolImplementation::new(
            "web_fetch",
            "client:web_fetch",
            NativeToolImplementationKind::Client,
            None,
            10,
        )
        .unwrap(),
        NativeToolImplementation::new(
            "code_execution",
            "openrouter:code_execution",
            NativeToolImplementationKind::Provider,
            Some("openrouter".to_owned()),
            10,
        )
        .unwrap(),
    ] {
        registry.register(&context, implementation).unwrap();
    }

    let prefer_local = NativeToolPolicy::new(
        NativeToolPolicyMode::PreferLocal,
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        registry
            .resolve_with_policy("openrouter", &prefer_local)
            .unwrap()[2]
            .implementation(),
        "client:web_search"
    );

    let local_only = NativeToolPolicy::new(
        NativeToolPolicyMode::PreferNative,
        [("web_search".to_owned(), NativeToolPolicyMode::LocalOnly)]
            .into_iter()
            .collect(),
    )
    .unwrap();
    assert_eq!(
        registry
            .resolve_with_policy("openrouter", &local_only)
            .unwrap()[2]
            .implementation(),
        "client:web_search"
    );

    let native_only_search = NativeToolPolicy::new(
        NativeToolPolicyMode::PreferLocal,
        [("web_search".to_owned(), NativeToolPolicyMode::NativeOnly)]
            .into_iter()
            .collect(),
    )
    .unwrap();
    assert_eq!(
        registry
            .resolve_with_policy("openrouter", &native_only_search)
            .unwrap()[2]
            .implementation(),
        "openrouter:web_search"
    );

    let local_only_unavailable = NativeToolPolicy::new(
        NativeToolPolicyMode::PreferNative,
        [("code_execution".to_owned(), NativeToolPolicyMode::LocalOnly)]
            .into_iter()
            .collect(),
    )
    .unwrap();
    let error = registry
        .resolve_with_policy("openrouter", &local_only_unavailable)
        .unwrap_err();
    assert!(error.to_string().contains("code_execution"));
    assert!(error.to_string().contains("local-only"));

    let native_only = NativeToolPolicy::new(
        NativeToolPolicyMode::NativeOnly,
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let error = registry
        .resolve_with_policy("openrouter", &native_only)
        .unwrap_err();
    assert!(error.to_string().contains("web_fetch"));
    assert!(error.to_string().contains("native-only"));

    let unknown = NativeToolPolicy::new(
        NativeToolPolicyMode::PreferNative,
        [(
            "image_generation".to_owned(),
            NativeToolPolicyMode::NativeOnly,
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    assert!(
        registry
            .resolve_with_policy("openrouter", &unknown)
            .unwrap_err()
            .to_string()
            .contains("image_generation")
    );
}

#[test]
fn settings_policy_plugin_applies_user_modes_and_disposes_to_default() {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            native_tool_policy_namespace().unwrap(),
            serde_json::json!({
                "default":"prefer-local",
                "overrides":{"web_fetch":"local-only"}
            }),
        )
        .unwrap();
    let plugins = vec![
        heycode_settings::settings_plugin(documents),
        native_tools_plugin(),
        native_tool_policy_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<NativeToolRegistry>(SERVICE_NATIVE_TOOLS)
        .unwrap();
    for implementation in [
        NativeToolImplementation::new(
            "web_search",
            "client:web_search",
            NativeToolImplementationKind::Client,
            None,
            10,
        )
        .unwrap(),
        NativeToolImplementation::new(
            "web_search",
            "openrouter:web_search",
            NativeToolImplementationKind::Provider,
            Some("openrouter".to_owned()),
            100,
        )
        .unwrap(),
        NativeToolImplementation::new(
            "web_fetch",
            "client:web_fetch",
            NativeToolImplementationKind::Client,
            None,
            10,
        )
        .unwrap(),
    ] {
        registry.register(&context, implementation).unwrap();
    }
    assert_eq!(
        registry.resolve("openrouter").unwrap()[1].implementation(),
        "client:web_search"
    );
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "native-tool-policy"
                && row.kind == heycode_core::ContributionKind::SettingsNamespace
                && row.name == "native-tools")
    );
    context.shutdown();
    assert!(registry.resolve("openrouter").unwrap().is_empty());
}

#[test]
fn model_scoped_native_candidates_fall_back_and_do_not_expand_authority() {
    let mut context = heycode_core::Context::new();
    let registry = NativeToolRegistry::new();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_search",
                "vendor:web_search",
                NativeToolImplementationKind::Provider,
                Some("vendor".into()),
                100,
            )
            .unwrap()
            .with_models(vec!["model-a".into()])
            .unwrap(),
        )
        .unwrap();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_search",
                "client:web_search",
                NativeToolImplementationKind::Client,
                None,
                10,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        registry.resolve_for_model("vendor", "model-a").unwrap()[0].implementation(),
        "vendor:web_search"
    );
    assert_eq!(
        registry.resolve_for_model("vendor", "model-b").unwrap()[0].implementation(),
        "client:web_search"
    );
    assert_eq!(
        registry.resolve_for_model("other", "model-a").unwrap()[0].implementation(),
        "client:web_search"
    );
    assert!(
        NativeToolImplementation::new(
            "web_search",
            "client:scoped",
            NativeToolImplementationKind::Client,
            None,
            0
        )
        .unwrap()
        .with_models(vec!["model-a".into()])
        .is_err()
    );
    context.shutdown();
    assert!(
        registry
            .resolve_for_model("vendor", "model-a")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn model_scope_never_turns_native_only_into_a_portable_fallback() {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            native_tool_policy_namespace().unwrap(),
            serde_json::json!({"default":"native-only"}),
        )
        .unwrap();
    let plugins = vec![
        heycode_settings::settings_plugin(documents),
        native_tools_plugin(),
        native_tool_policy_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let registry = context
        .get::<NativeToolRegistry>(SERVICE_NATIVE_TOOLS)
        .unwrap();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_search",
                "vendor:web_search",
                NativeToolImplementationKind::Provider,
                Some("vendor".into()),
                100,
            )
            .unwrap()
            .with_models(vec!["model-a".into()])
            .unwrap(),
        )
        .unwrap();
    registry
        .register(
            &context,
            NativeToolImplementation::new(
                "web_search",
                "client:web_search",
                NativeToolImplementationKind::Client,
                None,
                10,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        registry.resolve_for_model("vendor", "model-a").unwrap()[0].implementation(),
        "vendor:web_search"
    );
    let error = registry.resolve_for_model("vendor", "model-b").unwrap_err();
    assert!(error.to_string().contains("native-only"));
}

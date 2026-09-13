//! Settings-backed POA03 hosted-tool product policy.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::{ContributionKind, NativeToolImplementationKind};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEventStream};
use heycode_llm::{
    CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, Provider, ProviderOptionContext,
};
use heycode_provider_openai::{
    OPENAI_GPT_5_6_SOL, OPENAI_UNOWNED_UPPER_LOOP_KINDS, OpenAiConfiguredHostedToolPolicy,
    OpenAiHostedToolKind, OpenAiHostedToolSettingsFault, OpenAiProvider,
    openai_configured_native_tools_plugin, openai_hosted_tools_settings_namespace,
};
use heycode_settings::{SettingsApplies, SettingsDocuments, SettingsService, settings_plugin};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: OPENAI_GPT_5_6_SOL.to_owned(),
        display_name: OPENAI_GPT_5_6_SOL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            native_web: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn configured_value() -> serde_json::Value {
    serde_json::json!({
        "file_search":{
            "mode":"enabled",
            "vector_store_ids":["vs_product_docs","vs_runbooks"]
        },
        "remote_mcp":{
            "mode":"configured",
            "server_label":"knowledge",
            "server_url":"https://mcp.example.test/events",
            "require_approval":"always"
        }
    })
}

#[test]
fn one_restart_snapshot_drives_the_exact_plan_and_effect_owned_candidates() {
    let namespace = openai_hosted_tools_settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(namespace.clone(), configured_value())
        .unwrap();
    let plugins = vec![
        settings_plugin(documents),
        heycode_native_tools::native_tools_plugin(),
        openai_configured_native_tools_plugin(OPENAI_GPT_5_6_SOL),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    assert_eq!(
        snapshot.wire_projection().unwrap().resolved(),
        &configured_value()
    );

    let policy = OpenAiConfiguredHostedToolPolicy::resolve(&settings, OPENAI_GPT_5_6_SOL).unwrap();
    assert_eq!(
        policy.kinds(),
        vec![
            OpenAiHostedToolKind::WebSearch,
            OpenAiHostedToolKind::FileSearch,
            OpenAiHostedToolKind::CodeInterpreter,
            OpenAiHostedToolKind::HostedShell,
        ]
    );
    assert!(policy.configured_remote_mcp().is_some());
    assert_eq!(
        policy.unowned_upper_loop_kinds(),
        &OPENAI_UNOWNED_UPPER_LOOP_KINDS
    );

    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("openai").unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|route| (route.logical(), route.implementation(), route.kind()))
            .collect::<Vec<_>>(),
        vec![
            (
                "code_interpreter",
                "openai:code_interpreter",
                NativeToolImplementationKind::Provider,
            ),
            (
                "file_search",
                "openai:file_search",
                NativeToolImplementationKind::Provider,
            ),
            (
                "hosted_shell",
                "openai:hosted_shell",
                NativeToolImplementationKind::Provider,
            ),
            (
                "web_search",
                "openai:web_search",
                NativeToolImplementationKind::Provider,
            ),
        ]
    );
    assert!(routes.iter().all(|route| {
        !matches!(
            route.logical(),
            "remote_mcp" | "computer_use" | "image_generation"
        )
    }));

    let provider = policy
        .configure(
            OpenAiProvider::new(
                HttpService::new(Arc::new(DeadTransport)),
                "test-key",
                Some(OPENAI_GPT_5_6_SOL.to_owned()),
            )
            .unwrap(),
        )
        .unwrap();
    let options =
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &routes))
            .unwrap();
    let hosted = options
        .iter()
        .find(|option| option.kind() == "hosted-tools")
        .unwrap();
    assert_eq!(
        hosted.data()["definitions"],
        serde_json::json!([
            {"type":"web_search"},
            {"type":"file_search","vector_store_ids":["vs_product_docs","vs_runbooks"]},
            {"type":"code_interpreter","container":{"type":"auto"}},
            {
                "type":"shell",
                "allowed_callers":["direct"],
                "environment":{
                    "type":"container_auto",
                    "network_policy":{"type":"disabled"}
                }
            }
        ])
    );

    let inventory = context.plugin_inventory().snapshot().unwrap();
    let rows = inventory
        .contributions
        .iter()
        .filter(|row| row.plugin == "native-openai" && row.kind == ContributionKind::NativeTool)
        .map(|row| row.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            "openai:web_search",
            "openai:file_search",
            "openai:code_interpreter",
            "openai:hosted_shell",
        ]
    );

    context.shutdown();
    assert!(registry.resolve("openai").unwrap().is_empty());
    assert!(settings.get(&namespace).unwrap().is_none());
}

#[test]
fn defaults_are_explicit_and_keep_every_configuration_family_off() {
    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        heycode_native_tools::native_tools_plugin(),
        openai_configured_native_tools_plugin(OPENAI_GPT_5_6_SOL),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let namespace = openai_hosted_tools_settings_namespace().unwrap();
    let snapshot = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(
        snapshot.resolved(),
        &serde_json::json!({
            "file_search":{"mode":"disabled","vector_store_ids":[]},
            "remote_mcp":{
                "mode":"disabled",
                "server_label":"",
                "server_url":"",
                "require_approval":"always"
            }
        })
    );
    let policy = OpenAiConfiguredHostedToolPolicy::resolve(&settings, OPENAI_GPT_5_6_SOL).unwrap();
    assert_eq!(
        policy.kinds(),
        vec![
            OpenAiHostedToolKind::WebSearch,
            OpenAiHostedToolKind::CodeInterpreter,
            OpenAiHostedToolKind::HostedShell,
        ]
    );
    assert!(policy.configured_remote_mcp().is_none());
    context.shutdown();
}

#[test]
fn partial_duplicate_unsafe_and_unproven_configuration_fails_without_values() {
    let invalid = [
        serde_json::json!({
            "file_search":{"mode":"enabled","vector_store_ids":[]},
            "remote_mcp":{"mode":"disabled","server_label":"","server_url":"","require_approval":"always"}
        }),
        serde_json::json!({
            "file_search":{"mode":"enabled","vector_store_ids":["vs_one","vs_one"]},
            "remote_mcp":{"mode":"disabled","server_label":"","server_url":"","require_approval":"always"}
        }),
        serde_json::json!({
            "file_search":{"mode":"enabled","vector_store_ids":["unsafe id"]},
            "remote_mcp":{"mode":"disabled","server_label":"","server_url":"","require_approval":"always"}
        }),
        serde_json::json!({
            "file_search":{"mode":"disabled","vector_store_ids":[]},
            "remote_mcp":{"mode":"configured","server_label":"knowledge","server_url":"","require_approval":"always"}
        }),
        serde_json::json!({
            "file_search":{"mode":"disabled","vector_store_ids":[]},
            "remote_mcp":{"mode":"configured","server_label":"knowledge","server_url":"https://mcp.example.test/events","require_approval":"never"}
        }),
        serde_json::json!({
            "file_search":{"mode":"disabled","vector_store_ids":[]},
            "remote_mcp":{
                "mode":"configured","server_label":"knowledge",
                "server_url":"https://mcp.example.test/events",
                "require_approval":"always","authorization":"forbidden"
            }
        }),
    ];
    for value in invalid {
        assert_eq!(
            OpenAiConfiguredHostedToolPolicy::from_value(&value, OPENAI_GPT_5_6_SOL),
            Err(OpenAiHostedToolSettingsFault::InvalidSettings)
        );
    }
    assert_eq!(
        OpenAiConfiguredHostedToolPolicy::from_value(&configured_value(), "unlisted-model"),
        Err(OpenAiHostedToolSettingsFault::UnprovenCapability)
    );

    let canary = format!("sk-proj-{}", "A".repeat(64));
    let value = serde_json::json!({
        "file_search":{"mode":"disabled","vector_store_ids":[]},
        "remote_mcp":{
            "mode":"configured",
            "server_label":"knowledge",
            "server_url":format!("https://mcp.example.test/{canary}"),
            "require_approval":"always"
        }
    });
    let namespace = openai_hosted_tools_settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents.set_user(namespace, value).unwrap();
    let plugins = vec![
        settings_plugin(documents),
        heycode_native_tools::native_tools_plugin(),
        openai_configured_native_tools_plugin(OPENAI_GPT_5_6_SOL),
    ];
    let error = heycode_core::compose(&plugins).err().unwrap();
    assert!(!format!("{error:?} {error}").contains(&canary));
}

fn descriptor_for(id: &str) -> ModelDescriptor {
    ModelDescriptor {
        id: id.to_owned(),
        display_name: id.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities::unknown(),
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

#[test]
fn a_model_without_hosted_tool_evidence_composes_with_the_feature_absent() {
    for id in ["gpt-4o", "gpt-5.6-sol-mini", "not-a-real-openai-model"] {
        let plugins = vec![
            settings_plugin(SettingsDocuments::new()),
            heycode_native_tools::native_tools_plugin(),
            openai_configured_native_tools_plugin(id),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let settings = context
            .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .unwrap();
        let namespace = openai_hosted_tools_settings_namespace().unwrap();
        assert!(settings.get(&namespace).unwrap().is_some());

        let policy = OpenAiConfiguredHostedToolPolicy::resolve(&settings, id).unwrap();
        assert_eq!(policy.kinds(), Vec::new());
        assert!(policy.configured_remote_mcp().is_none());

        let registry = context
            .get::<heycode_native_tools::NativeToolRegistry>(
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            )
            .unwrap();
        let routes = registry.resolve("openai").unwrap();
        assert!(routes.is_empty());
        let inventory = context.plugin_inventory().snapshot().unwrap();
        assert!(inventory.contributions.iter().all(|row| {
            row.plugin != "native-openai" || row.kind != ContributionKind::NativeTool
        }));

        let provider = policy
            .configure(
                OpenAiProvider::new(
                    HttpService::new(Arc::new(DeadTransport)),
                    "test-key",
                    Some(id.to_owned()),
                )
                .unwrap(),
            )
            .unwrap();
        let options = Provider::request_options_for(
            &provider,
            ProviderOptionContext::new(&descriptor_for(id), &routes),
        )
        .unwrap();
        assert!(options.iter().all(|option| option.kind() != "hosted-tools"));
        context.shutdown();
    }
}

#[test]
fn explicitly_enabling_a_hosted_family_on_an_unproven_model_still_fails_closed() {
    let file_search_only = serde_json::json!({
        "file_search":{"mode":"enabled","vector_store_ids":["vs_product_docs"]},
        "remote_mcp":{
            "mode":"disabled","server_label":"","server_url":"","require_approval":"always"
        }
    });
    let remote_mcp_only = serde_json::json!({
        "file_search":{"mode":"disabled","vector_store_ids":[]},
        "remote_mcp":{
            "mode":"configured","server_label":"knowledge",
            "server_url":"https://mcp.example.test/events","require_approval":"always"
        }
    });
    for id in ["gpt-4o", "gpt-5.6-sol-mini", "not-a-real-openai-model"] {
        for value in [&configured_value(), &file_search_only, &remote_mcp_only] {
            assert_eq!(
                OpenAiConfiguredHostedToolPolicy::from_value(value, id),
                Err(OpenAiHostedToolSettingsFault::UnprovenCapability)
            );
        }
    }
}

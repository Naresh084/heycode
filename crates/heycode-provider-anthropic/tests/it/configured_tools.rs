//! Settings-backed PAN03 server-tool product policy.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::{ContributionKind, NativeToolImplementationKind};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEventStream};
use heycode_llm::{
    CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, Provider, ProviderOptionContext,
};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_EXCLUDED_DEFAULT_SERVER_TOOL_KINDS,
    AnthropicConfiguredServerToolPolicy, AnthropicProvider, AnthropicServerToolKind,
    AnthropicServerToolSettingsFault, anthropic_configured_native_tools_plugin,
    anthropic_server_tools_settings_namespace, server_tool_support,
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
        id: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        display_name: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn configured_value() -> serde_json::Value {
    serde_json::json!({
        "advisor":{"mode":"enabled","model":"claude-opus-5","max_uses":2},
        "tool_search":{
            "mode":"enabled",
            "deferred_tool_names":["read","grep"]
        },
        "mcp":{
            "mode":"enabled",
            "servers":[
                {"name":"knowledge","url":"https://mcp.example.test/events"},
                {"name":"issues","url":"https://issues.example.test/mcp"}
            ]
        }
    })
}

#[test]
fn one_restart_snapshot_drives_the_exact_plan_and_effect_owned_candidates() {
    let namespace = anthropic_server_tools_settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(namespace.clone(), configured_value())
        .unwrap();
    let plugins = vec![
        settings_plugin(documents),
        heycode_native_tools::native_tools_plugin(),
        anthropic_configured_native_tools_plugin(ANTHROPIC_CLAUDE_OPUS_5),
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

    let policy =
        AnthropicConfiguredServerToolPolicy::resolve(&settings, ANTHROPIC_CLAUDE_OPUS_5).unwrap();
    assert_eq!(
        policy.kinds(),
        vec![
            AnthropicServerToolKind::WebSearch,
            AnthropicServerToolKind::CodeExecution,
            AnthropicServerToolKind::Advisor,
            AnthropicServerToolKind::ToolSearch,
            AnthropicServerToolKind::McpConnector,
        ]
    );
    assert_eq!(
        policy.excluded_default_kinds(),
        &ANTHROPIC_EXCLUDED_DEFAULT_SERVER_TOOL_KINDS
    );
    assert_eq!(
        server_tool_support(ANTHROPIC_CLAUDE_OPUS_5, AnthropicServerToolKind::WebFetch),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        policy.plan().unwrap().deferred_tool_names(),
        &["read".to_owned(), "grep".to_owned()]
    );

    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("anthropic").unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|route| (route.logical(), route.implementation(), route.kind()))
            .collect::<Vec<_>>(),
        vec![
            (
                "advisor",
                "anthropic:advisor",
                NativeToolImplementationKind::Provider,
            ),
            (
                "code_execution",
                "anthropic:code_execution",
                NativeToolImplementationKind::Provider,
            ),
            (
                "remote_mcp",
                "anthropic:remote_mcp",
                NativeToolImplementationKind::Provider,
            ),
            (
                "tool_search",
                "anthropic:tool_search",
                NativeToolImplementationKind::Provider,
            ),
            (
                "web_search",
                "anthropic:web_search",
                NativeToolImplementationKind::Provider,
            ),
        ]
    );
    assert!(routes.iter().all(|route| route.logical() != "web_fetch"));

    let provider = policy
        .configure(
            AnthropicProvider::new(
                HttpService::new(Arc::new(DeadTransport)),
                "test-key",
                Some(ANTHROPIC_CLAUDE_OPUS_5.to_owned()),
            )
            .unwrap(),
        )
        .unwrap();
    let options =
        Provider::request_options_for(&provider, ProviderOptionContext::new(&model(), &routes))
            .unwrap();
    let server_tools = options
        .iter()
        .find(|option| option.kind() == "server-tools")
        .unwrap();
    assert_eq!(
        server_tools.data()["tools"],
        serde_json::json!([
            {"type":"web_search_20250305","name":"web_search","max_uses":5},
            {"type":"code_execution_20260521","name":"code_execution"},
            {"type":"advisor_20260301","name":"advisor","model":"claude-opus-5","max_uses":2},
            {"type":"tool_search_tool_regex_20251119","name":"tool_search_tool_regex"},
            {
                "type":"mcp_toolset",
                "mcp_server_name":"knowledge",
                "default_config":{"enabled":true,"defer_loading":false}
            },
            {
                "type":"mcp_toolset",
                "mcp_server_name":"issues",
                "default_config":{"enabled":true,"defer_loading":false}
            }
        ])
    );
    assert_eq!(
        server_tools.data()["request"]["mcp_servers"],
        serde_json::json!([
            {"type":"url","url":"https://mcp.example.test/events","name":"knowledge"},
            {"type":"url","url":"https://issues.example.test/mcp","name":"issues"}
        ])
    );
    assert_eq!(
        server_tools.data()["deferred_tool_names"],
        serde_json::json!(["grep", "read"])
    );

    let inventory = context.plugin_inventory().snapshot().unwrap();
    let rows = inventory
        .contributions
        .iter()
        .filter(|row| row.plugin == "native-anthropic" && row.kind == ContributionKind::NativeTool)
        .map(|row| row.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            "anthropic:web_search",
            "anthropic:code_execution",
            "anthropic:advisor",
            "anthropic:tool_search",
            "anthropic:remote_mcp",
        ]
    );

    context.shutdown();
    assert!(registry.resolve("anthropic").unwrap().is_empty());
    assert!(settings.get(&namespace).unwrap().is_none());
}

#[test]
fn defaults_are_explicit_and_preserve_the_supported_atomic_baseline_only() {
    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        heycode_native_tools::native_tools_plugin(),
        anthropic_configured_native_tools_plugin(ANTHROPIC_CLAUDE_OPUS_5),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let namespace = anthropic_server_tools_settings_namespace().unwrap();
    let snapshot = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(
        snapshot.resolved(),
        &serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"disabled","servers":[]}
        })
    );
    let policy =
        AnthropicConfiguredServerToolPolicy::resolve(&settings, ANTHROPIC_CLAUDE_OPUS_5).unwrap();
    assert_eq!(
        policy.kinds(),
        vec![
            AnthropicServerToolKind::WebSearch,
            AnthropicServerToolKind::CodeExecution,
        ]
    );
    context.shutdown();
}

#[test]
fn partial_duplicate_unsafe_unsupported_and_unproven_configuration_fails_closed() {
    let invalid = [
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":2},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"enabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"enabled","model":"claude-opus-5","max_uses":0},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"enabled","deferred_tool_names":[]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"enabled","deferred_tool_names":["read","read"]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"enabled","deferred_tool_names":["unsafe.name"]},
            "mcp":{"mode":"disabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"enabled","servers":[]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"enabled","servers":[
                {"name":"knowledge","url":"https://mcp.example.test/one"},
                {"name":"knowledge","url":"https://mcp.example.test/two"}
            ]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"enabled","servers":[
                {"name":"one","url":"https://mcp.example.test/shared"},
                {"name":"two","url":"https://mcp.example.test/shared"}
            ]}
        }),
        serde_json::json!({
            "advisor":{"mode":"disabled","model":"","max_uses":1},
            "tool_search":{"mode":"disabled","deferred_tool_names":[]},
            "mcp":{"mode":"enabled","servers":[{
                "name":"knowledge","url":"https://mcp.example.test/events",
                "authorization_token":"forbidden"
            }]}
        }),
    ];
    for value in invalid {
        assert_eq!(
            AnthropicConfiguredServerToolPolicy::from_value(&value, ANTHROPIC_CLAUDE_OPUS_5,),
            Err(AnthropicServerToolSettingsFault::InvalidSettings)
        );
    }

    let mut unsupported = configured_value();
    unsupported["advisor"]["model"] = serde_json::json!("claude-opus-4-8");
    assert_eq!(
        AnthropicConfiguredServerToolPolicy::from_value(&unsupported, ANTHROPIC_CLAUDE_OPUS_5,),
        Err(AnthropicServerToolSettingsFault::UnsupportedCapability)
    );
    let mut unproven = configured_value();
    unproven["advisor"]["model"] = serde_json::json!("unknown-advisor");
    assert_eq!(
        AnthropicConfiguredServerToolPolicy::from_value(&unproven, ANTHROPIC_CLAUDE_OPUS_5,),
        Err(AnthropicServerToolSettingsFault::UnprovenCapability)
    );
    assert_eq!(
        AnthropicConfiguredServerToolPolicy::from_value(&configured_value(), "unknown-executor"),
        Err(AnthropicServerToolSettingsFault::UnprovenCapability)
    );

    let canary = format!("sk-ant-api03-{}", "A".repeat(64));
    let value = serde_json::json!({
        "advisor":{"mode":"disabled","model":"","max_uses":1},
        "tool_search":{"mode":"disabled","deferred_tool_names":[]},
        "mcp":{"mode":"enabled","servers":[{
            "name":"knowledge",
            "url":format!("https://mcp.example.test/{canary}")
        }]}
    });
    let namespace = anthropic_server_tools_settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents.set_user(namespace, value).unwrap();
    let plugins = vec![
        settings_plugin(documents),
        heycode_native_tools::native_tools_plugin(),
        anthropic_configured_native_tools_plugin(ANTHROPIC_CLAUDE_OPUS_5),
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
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

#[test]
fn an_executor_without_server_tool_evidence_composes_with_only_proven_families() {
    let cases: [(&str, Vec<AnthropicServerToolKind>); 3] = [
        (
            "claude-sonnet-5",
            vec![AnthropicServerToolKind::CodeExecution],
        ),
        ("claude-haiku-5", Vec::new()),
        ("not-a-real-anthropic-model", Vec::new()),
    ];
    for (id, expected) in cases {
        let plugins = vec![
            settings_plugin(SettingsDocuments::new()),
            heycode_native_tools::native_tools_plugin(),
            anthropic_configured_native_tools_plugin(id),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let settings = context
            .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .unwrap();
        let namespace = anthropic_server_tools_settings_namespace().unwrap();
        assert!(settings.get(&namespace).unwrap().is_some());

        let policy = AnthropicConfiguredServerToolPolicy::resolve(&settings, id).unwrap();
        assert_eq!(policy.kinds(), expected);
        assert_eq!(policy.plan().is_some(), !expected.is_empty());

        let registry = context
            .get::<heycode_native_tools::NativeToolRegistry>(
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
            )
            .unwrap();
        let routes = registry.resolve("anthropic").unwrap();
        assert_eq!(
            routes
                .iter()
                .map(|route| route.logical())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
        );

        let provider = policy
            .configure(
                AnthropicProvider::new(
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
        let server_tools = options
            .iter()
            .find(|option| option.kind() == "server-tools");
        match server_tools {
            Some(option) => assert_eq!(
                option.data()["tools"],
                serde_json::json!([{"type":"code_execution_20260521","name":"code_execution"}])
            ),
            None => assert!(expected.is_empty()),
        }
        context.shutdown();
    }
}

#[test]
fn explicitly_enabling_a_server_tool_family_on_an_unproven_executor_still_fails_closed() {
    let tool_search_only = serde_json::json!({
        "advisor":{"mode":"disabled","model":"","max_uses":1},
        "tool_search":{"mode":"enabled","deferred_tool_names":["read","grep"]},
        "mcp":{"mode":"disabled","servers":[]}
    });
    let mcp_only = serde_json::json!({
        "advisor":{"mode":"disabled","model":"","max_uses":1},
        "tool_search":{"mode":"disabled","deferred_tool_names":[]},
        "mcp":{"mode":"enabled","servers":[
            {"name":"knowledge","url":"https://mcp.example.test/events"}
        ]}
    });
    for id in [
        "claude-sonnet-5",
        "claude-haiku-5",
        "not-a-real-anthropic-model",
    ] {
        for value in [&configured_value(), &mcp_only, &tool_search_only] {
            assert_eq!(
                AnthropicConfiguredServerToolPolicy::from_value(value, id),
                Err(AnthropicServerToolSettingsFault::UnprovenCapability)
            );
        }
    }
}

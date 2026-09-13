//! K04 `/plugins verbose` real-composition attribution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_cli::testing::RealCompositionHarness;

#[tokio::test]
async fn plugins_verbose_attributes_services_tools_commands_and_provider_rows() {
    let world = RealCompositionHarness::new().unwrap().compose().unwrap();
    let context = world.context();
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert_eq!(inventory.plugins.len(), context.plugins().len());
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "tools"
            && row.kind == heycode_core::ContributionKind::Tool
            && row.name == "read"
    }));
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "catalog-deepseek"
            && row.kind == heycode_core::ContributionKind::ModelCatalog
            && row.name == "deepseek"
    }));
    for (plugin, kind, name) in [
        (
            "doctor-config",
            heycode_core::ContributionKind::DoctorCheck,
            "config-migration",
        ),
        (
            "doctor-settings",
            heycode_core::ContributionKind::DoctorCheck,
            "settings",
        ),
        (
            "doctor-credentials",
            heycode_core::ContributionKind::DoctorCheck,
            "credentials",
        ),
        (
            "settings-file",
            heycode_core::ContributionKind::SettingsProvider,
            "file",
        ),
        (
            "settings-aws-bedrock",
            heycode_core::ContributionKind::SettingsNamespace,
            "aws-bedrock",
        ),
        (
            "settings-google-inference",
            heycode_core::ContributionKind::SettingsNamespace,
            "google-inference",
        ),
        (
            "credentials",
            heycode_core::ContributionKind::SettingsNamespace,
            "credentials",
        ),
        (
            "credentials-env",
            heycode_core::ContributionKind::CredentialProvider,
            "environment",
        ),
        (
            "credentials-command",
            heycode_core::ContributionKind::CredentialProvider,
            "command",
        ),
        (
            "authorization-api-key",
            heycode_core::ContributionKind::AuthorizationFlow,
            "deepseek-api-key",
        ),
        (
            "provider-openrouter",
            heycode_core::ContributionKind::AuthorizationFlow,
            "openrouter-api-key",
        ),
        (
            "provider-anthropic",
            heycode_core::ContributionKind::AuthorizationFlow,
            "anthropic-api-key",
        ),
        (
            "catalog-openrouter",
            heycode_core::ContributionKind::ModelCatalog,
            "openrouter",
        ),
        (
            "tools",
            heycode_core::ContributionKind::NativeTool,
            "client:web_fetch",
        ),
        (
            "tools",
            heycode_core::ContributionKind::NativeTool,
            "client:web_search",
        ),
        (
            "native-openai",
            heycode_core::ContributionKind::SettingsNamespace,
            "openai-hosted-tools",
        ),
        (
            "native-openai",
            heycode_core::ContributionKind::NativeTool,
            "openai:web_search",
        ),
        (
            "native-openai",
            heycode_core::ContributionKind::NativeTool,
            "openai:code_interpreter",
        ),
        (
            "native-openai",
            heycode_core::ContributionKind::NativeTool,
            "openai:hosted_shell",
        ),
        (
            "native-anthropic",
            heycode_core::ContributionKind::SettingsNamespace,
            "anthropic-server-tools",
        ),
        (
            "native-anthropic",
            heycode_core::ContributionKind::NativeTool,
            "anthropic:web_search",
        ),
        (
            "native-anthropic",
            heycode_core::ContributionKind::NativeTool,
            "anthropic:code_execution",
        ),
        (
            "native-openrouter",
            heycode_core::ContributionKind::NativeTool,
            "openrouter:web_search",
        ),
        (
            "native-tool-policy",
            heycode_core::ContributionKind::SettingsNamespace,
            "native-tools",
        ),
        (
            "web-portable",
            heycode_core::ContributionKind::WebProvider,
            "portable",
        ),
        (
            "web-extract",
            heycode_core::ContributionKind::WebProcessor,
            "portable-readable",
        ),
        (
            "web-policy",
            heycode_core::ContributionKind::SettingsNamespace,
            "web",
        ),
        (
            "lsp-tools",
            heycode_core::ContributionKind::Tool,
            "lsp_servers",
        ),
        (
            "lsp-tools",
            heycode_core::ContributionKind::Tool,
            "lsp_definition",
        ),
        (
            "lsp-tools",
            heycode_core::ContributionKind::Tool,
            "lsp_references",
        ),
        (
            "lsp-tools",
            heycode_core::ContributionKind::Tool,
            "lsp_diagnostics",
        ),
        ("status-web", heycode_core::ContributionKind::Command, "web"),
        (
            "status-context",
            heycode_core::ContributionKind::Command,
            "context",
        ),
        (
            "status-context",
            heycode_core::ContributionKind::Command,
            "usage",
        ),
        (
            "health-history",
            heycode_core::ContributionKind::Command,
            "health",
        ),
        (
            "lmstudio-control",
            heycode_core::ContributionKind::SettingsNamespace,
            "lmstudio-load",
        ),
        (
            "provider-anthropic",
            heycode_core::ContributionKind::SettingsNamespace,
            "anthropic",
        ),
        (
            "provider-openai",
            heycode_core::ContributionKind::SettingsNamespace,
            "openai-prompt-cache",
        ),
        (
            "lmstudio-control",
            heycode_core::ContributionKind::Command,
            "lmstudio",
        ),
        ("tui", heycode_core::ContributionKind::Command, "profile"),
        ("tui", heycode_core::ContributionKind::Command, "diff"),
        ("tui", heycode_core::ContributionKind::Command, "copy"),
        ("tui", heycode_core::ContributionKind::Command, "mention"),
        ("tui", heycode_core::ContributionKind::Command, "review"),
        ("tui", heycode_core::ContributionKind::Command, "theme"),
        ("tui", heycode_core::ContributionKind::Command, "keymap"),
        ("tui", heycode_core::ContributionKind::Command, "vim"),
        ("tui", heycode_core::ContributionKind::Command, "settings"),
        (
            "tui",
            heycode_core::ContributionKind::SettingsNamespace,
            "keymap",
        ),
        (
            "tui",
            heycode_core::ContributionKind::SettingsNamespace,
            "ui-preferences",
        ),
        ("tui", heycode_core::ContributionKind::Command, "new"),
        ("tui", heycode_core::ContributionKind::Command, "resume"),
        ("tui", heycode_core::ContributionKind::Command, "fork"),
        ("tui", heycode_core::ContributionKind::Command, "rename"),
        ("tui", heycode_core::ContributionKind::Command, "archive"),
        ("tui", heycode_core::ContributionKind::Command, "delete"),
        ("tui", heycode_core::ContributionKind::Command, "export"),
        (
            "agent-attachments",
            heycode_core::ContributionKind::Command,
            "attach",
        ),
        (
            "agent-documents",
            heycode_core::ContributionKind::Command,
            "document",
        ),
        (
            "tools",
            heycode_core::ContributionKind::InterceptionSeam,
            "seam/pre_tool",
        ),
        (
            "llm",
            heycode_core::ContributionKind::InterceptionSeam,
            "provider/request",
        ),
        (
            "llm",
            heycode_core::ContributionKind::InterceptionSeam,
            "provider/response",
        ),
        (
            "request-transforms",
            heycode_core::ContributionKind::InterceptionLayer,
            "provider/request:transforms",
        ),
        (
            "request-transforms-openrouter",
            heycode_core::ContributionKind::RequestTransform,
            "openrouter:context-compression",
        ),
        (
            "request-transforms-openrouter",
            heycode_core::ContributionKind::RequestTransform,
            "openrouter:file-parser",
        ),
        (
            "request-transforms-openrouter",
            heycode_core::ContributionKind::RequestTransform,
            "openrouter:response-healing",
        ),
        (
            "agent",
            heycode_core::ContributionKind::InterceptionLayer,
            "provider/request:authentication",
        ),
        (
            "agent",
            heycode_core::ContributionKind::InterceptionLayer,
            "provider/request:native-tools",
        ),
        (
            "provider-telemetry",
            heycode_core::ContributionKind::InterceptionLayer,
            "provider/response:telemetry",
        ),
        (
            "telemetry-metrics",
            heycode_core::ContributionKind::TelemetryMetric,
            "provider_request",
        ),
        (
            "telemetry-metrics",
            heycode_core::ContributionKind::TelemetryMetric,
            "tool",
        ),
        (
            "telemetry-metrics",
            heycode_core::ContributionKind::TelemetryMetric,
            "compaction",
        ),
        (
            "telemetry-metrics",
            heycode_core::ContributionKind::TelemetryMetric,
            "cache",
        ),
        (
            "plan",
            heycode_core::ContributionKind::InterceptionLayer,
            "seam/pre_tool:plan-guard",
        ),
        (
            "execution-jobs",
            heycode_core::ContributionKind::Service,
            "execution-jobs",
        ),
        (
            "execution-jobs",
            heycode_core::ContributionKind::Command,
            "tasks",
        ),
        (
            "execution-jobs",
            heycode_core::ContributionKind::Command,
            "ps",
        ),
        (
            "execution-jobs",
            heycode_core::ContributionKind::Command,
            "stop",
        ),
        (
            "execution-jobs",
            heycode_core::ContributionKind::Tool,
            "background_shell",
        ),
        (
            "execution-jobs",
            heycode_core::ContributionKind::Tool,
            "background_terminal",
        ),
        ("goals", heycode_core::ContributionKind::Service, "goals"),
        ("goals", heycode_core::ContributionKind::Command, "goal"),
        ("goals", heycode_core::ContributionKind::Tool, "goal"),
        (
            "workflows",
            heycode_core::ContributionKind::Service,
            "workflows",
        ),
        (
            "workflows",
            heycode_core::ContributionKind::Tool,
            "workflow",
        ),
        (
            "schedules",
            heycode_core::ContributionKind::Service,
            "schedules",
        ),
        (
            "schedules",
            heycode_core::ContributionKind::Tool,
            "schedule_create",
        ),
        (
            "schedules",
            heycode_core::ContributionKind::Tool,
            "schedule_list",
        ),
        (
            "schedules",
            heycode_core::ContributionKind::Tool,
            "schedule_delete",
        ),
        ("teams", heycode_core::ContributionKind::Service, "teams"),
        ("teams", heycode_core::ContributionKind::Tool, "team"),
        (
            "reviewer",
            heycode_core::ContributionKind::Service,
            "reviews",
        ),
        (
            "reviewer",
            heycode_core::ContributionKind::Command,
            "review-runtime",
        ),
        (
            "deferred-tools",
            heycode_core::ContributionKind::InterceptionLayer,
            "agent/adapter-preparation:deferred-tools",
        ),
        (
            "loop-budget-settings",
            heycode_core::ContributionKind::SettingsNamespace,
            "loop-budget",
        ),
        (
            "loop-budget-settings",
            heycode_core::ContributionKind::InterceptionLayer,
            "agent/pre-step:loop-budget",
        ),
        (
            "skills",
            heycode_core::ContributionKind::SettingsNamespace,
            "skills-preferences",
        ),
        (
            "runtime-claude",
            heycode_core::ContributionKind::AgentRuntime,
            "claude",
        ),
        (
            "runtime-codex",
            heycode_core::ContributionKind::AgentRuntime,
            "codex",
        ),
        (
            "runtime-opencode",
            heycode_core::ContributionKind::AgentRuntime,
            "opencode",
        ),
        (
            "runtime-deepseek-harness",
            heycode_core::ContributionKind::AgentRuntime,
            "deepseek-harness",
        ),
        (
            "runtime-native",
            heycode_core::ContributionKind::AgentRuntime,
            "native",
        ),
        (
            "routing",
            heycode_core::ContributionKind::SettingsNamespace,
            "routing",
        ),
        (
            "mcp-management",
            heycode_core::ContributionKind::Service,
            "mcp-management",
        ),
        (
            "mcp-management",
            heycode_core::ContributionKind::SettingsNamespace,
            "mcp-servers",
        ),
        (
            "plugin-lifecycle",
            heycode_core::ContributionKind::Service,
            "plugin-lifecycle",
        ),
        (
            "plugin-lifecycle",
            heycode_core::ContributionKind::SettingsNamespace,
            "plugins",
        ),
        (
            "panel-commands",
            heycode_core::ContributionKind::Command,
            "mcp",
        ),
        (
            "panel-commands",
            heycode_core::ContributionKind::Command,
            "agents",
        ),
        (
            "panel-commands",
            heycode_core::ContributionKind::Command,
            "hooks",
        ),
        ("tui", heycode_core::ContributionKind::UserInterface, "tui"),
        (
            "tui",
            heycode_core::ContributionKind::UiSlot,
            "panel:transcript",
        ),
        (
            "tui",
            heycode_core::ContributionKind::UiSlot,
            "panel:sessions",
        ),
        (
            "tui",
            heycode_core::ContributionKind::UiSlot,
            "panel:skills",
        ),
        (
            "tui",
            heycode_core::ContributionKind::UiSlot,
            "panel:agents",
        ),
        ("tui", heycode_core::ContributionKind::UiSlot, "panel:hooks"),
        (
            "tui",
            heycode_core::ContributionKind::UiSlot,
            "dialog:approval",
        ),
        (
            "tui",
            heycode_core::ContributionKind::UiSlot,
            "status:session",
        ),
    ] {
        assert!(
            inventory
                .contributions
                .iter()
                .any(|row| row.plugin == plugin && row.kind == kind && row.name == name),
            "missing {plugin} {kind}: {name}"
        );
    }

    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let command = commands
        .get("plugins")
        .unwrap()
        .expect("/plugins must be registered");
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        captured.lock().unwrap().push(event.clone());
    });
    command.execute(&agent, "verbose").await.unwrap();
    let text = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            heycode_agent::UiEvent::Info { text } if text.starts_with("plugins:") => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("verbose inventory output");
    for expected in [
        "plugin tools@",
        "plugin doctor-settings@",
        "plugin doctor-config@",
        "plugin subprocess-local@",
        "plugin shell-local@",
        "plugin sandbox@",
        "doctor_check: config-migration",
        "doctor_check: settings",
        "scope=built_in",
        "service: tools",
        "service: subprocess",
        "service: shell",
        "service: sandbox",
        "tool: read",
        "plugin lsp-tools@",
        "tool: lsp_servers",
        "tool: lsp_definition",
        "plugin skills@",
        "settings_namespace: skills-preferences",
        "plugin session-query-jsonl@",
        "plugin attachments-local@",
        "service: session-query",
        "command: skills",
        "plugin catalog-deepseek@",
        "plugin credentials-command@",
        "credential_provider: command",
        "model_catalog: deepseek",
        "plugin catalog-overrides@",
        "service: catalog-overrides",
        "inference_provider: fake",
        "plugin runtimes@",
        "service: runtimes",
        "plugin runtime-claude@",
        "agent_runtime: claude",
        "plugin runtime-codex@",
        "agent_runtime: codex",
        "plugin runtime-opencode@",
        "agent_runtime: opencode",
        "plugin runtime-deepseek-harness@",
        "agent_runtime: deepseek-harness",
        "plugin runtime-native@",
        "agent_runtime: native",
        "plugin routing@",
        "plugin routing-auth@",
        "service: routing",
        "settings_namespace: routing",
        "command: connect",
        "command: logout",
        "command: effort",
        "plugin lmstudio-control@",
        "settings_namespace: lmstudio-load",
        "command: lmstudio",
        "command: plugins",
        "plugin deferred-tools@",
        "interception_layer: agent/adapter-preparation:deferred-tools",
        "plugin loop-budget-settings@",
        "settings_namespace: loop-budget",
        "plugin execution-jobs@",
        "service: execution-jobs",
        "command: tasks",
        "command: ps",
        "command: stop",
        "tool: background_shell",
        "tool: background_terminal",
        "plugin goals@",
        "service: goals",
        "command: goal",
        "tool: goal",
        "plugin workflows@",
        "service: workflows",
        "tool: workflow",
        "plugin schedules@",
        "service: schedules",
        "tool: schedule_create",
        "tool: schedule_list",
        "tool: schedule_delete",
        "plugin teams@",
        "service: teams",
        "tool: team",
        "plugin reviewer@",
        "service: reviews",
        "command: review-runtime",
        "plugin init@",
        "command: init",
        "ui_slot: panel:transcript",
        "ui_slot: panel:skills",
        "ui_slot: panel:agents",
        "ui_slot: panel:hooks",
        "ui_slot: dialog:approval",
        "ui_slot: status:session",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    world.shutdown();
}

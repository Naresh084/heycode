//! Regressions for architectural-law violations found in the AGENTS.md audit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, AutoApprove, Command, CommandRegistry, agent_options_plugin, agent_plugin,
    approval_plugin, commands_plugin, subagent_plugin,
};
use heycode_core::{CoreError, Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, Provider, StreamChunk, llm_plugin, model_catalog_plugin};
use heycode_prompt::prompt_plugin;
use heycode_session::session_plugin;
use heycode_tools::tools_plugin;

fn execution_plugin() -> Box<dyn heycode_core::Plugin> {
    heycode_exec::local_execution_plugin(
        heycode_exec::LocalShellConfig::platform(
            std::env::current_dir().unwrap(),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
    )
}

fn stop_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

/// Principle #4: a plugin must DECLARE every service its `apply` reads, so an
/// absent one fails as a named unsatisfied inject rather than a runtime
/// `CoreError::other` from deep inside `apply`.
#[test]
fn subagent_plugin_declares_the_session_it_requires() {
    let dir = tempfile::tempdir().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![stop_text("ok")]));
    // Everything the subagent plugin declares EXCEPT session.
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        subagent_plugin(dir.path().to_path_buf(), 3),
    ];
    let Err(err) = compose(&plugins) else {
        panic!("composing without a session must fail");
    };
    match err {
        CoreError::UnsatisfiedInject { plugin, missing } => {
            assert_eq!(plugin, "subagent");
            assert!(
                missing.iter().any(|m| m == "session"),
                "must name session: {missing:?}"
            );
        }
        other => panic!("expected UnsatisfiedInject, got {other:?}"),
    }
}

struct Late(heycode_agent::CommandDescriptor);
#[async_trait::async_trait]
impl Command for Late {
    fn descriptor(&self) -> &heycode_agent::CommandDescriptor {
        &self.0
    }
    async fn execute(&self, _agent: &heycode_agent::Agent, _args: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// `/help` printed a hardcoded list, so every `register_shared` command
/// (`/skills`, `/skill`, `/plan`) was invisible to the user.
#[tokio::test]
async fn help_lists_late_registered_commands() {
    let dir = tempfile::tempdir().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![stop_text("ok")]));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(dir.path().to_path_buf()),
        prompt_plugin(),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = ctx
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    commands
        .register_shared(Arc::new(Late(
            heycode_agent::CommandDescriptor::new(
                "deploy",
                "ship the thing",
                Vec::new(),
                heycode_agent::CommandTiming::Immediate,
                heycode_agent::CommandSource::from_plugin("test").unwrap(),
            )
            .unwrap(),
        )))
        .unwrap();

    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    ctx.events.on::<heycode_agent::UiEvent>(move |event| {
        if let heycode_agent::UiEvent::HelpRequested { header, commands } = event {
            let text = format!(
                "{header}\n{}",
                commands
                    .iter()
                    .map(heycode_agent::CommandCatalogEntry::help_line)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            sink.lock().unwrap_or_else(|p| p.into_inner()).push(text);
        }
    });

    commands
        .get("help")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();

    let text = seen.lock().unwrap_or_else(|p| p.into_inner()).join("\n");
    assert!(
        text.contains("/deploy"),
        "late command missing from /help: {text}"
    );
    assert!(text.contains("ship the thing"), "help text missing: {text}");
    // The builtins must still be there.
    for builtin in ["/help", "/compact", "/title", "/quit"] {
        assert!(text.contains(builtin), "builtin {builtin} missing: {text}");
    }
}

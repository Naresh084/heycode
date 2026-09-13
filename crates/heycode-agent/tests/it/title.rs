//! Session titles: opt-in generation + manual `/title`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{AutoApprove, agent_plugin, approval_plugin, commands_plugin, parse_slash};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, StreamChunk, llm_plugin, model_catalog_plugin};
use heycode_prompt::prompt_plugin;
use heycode_session::{Session, SessionEventKind, session_plugin};
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

fn options_plugin(auto_title: bool) -> Box<dyn Plugin> {
    struct Opts(bool);
    impl Plugin for Opts {
        fn name(&self) -> &'static str {
            "agent-options"
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
            ctx.provide(
                heycode_agent::SERVICE_AGENT_OPTIONS,
                "agent-options",
                heycode_agent::AgentOptions {
                    compaction: heycode_agent::CompactionPolicy::default(),
                    max_task_depth: 3,
                    cwd: None,
                    auto_title: self.0,
                },
            )
        }
    }
    Box::new(Opts(auto_title))
}

#[tokio::test]
async fn auto_title_runs_once_after_first_turn_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![
        stop_text("working on it"),
        stop_text("Fix Login Redirect Bug"),
    ]));
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
        options_plugin(true),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    agent
        .send("the login redirect breaks on safari")
        .await
        .unwrap();

    // The titler is fire-and-forget; poll briefly for the durable event.
    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let mut title = None;
    for _ in 0..50 {
        {
            let s = session.lock().unwrap_or_else(|e| e.into_inner());
            title = heycode_agent::title::current(s.events());
        }
        if title.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(title.as_deref(), Some("Fix Login Redirect Bug"));
}

#[tokio::test]
async fn disabled_policy_never_spends_a_provider_call() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![stop_text("answer")]));
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
        options_plugin(false),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("hi").await.unwrap();

    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        !s.events()
            .iter()
            .any(|e| matches!(e.kind, SessionEventKind::SessionTitle { .. }))
    );
}

#[tokio::test]
async fn manual_title_command_sets_and_shows() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![stop_text("ok")]));
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
        options_plugin(false),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("hello").await.unwrap();

    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let (name, args) = parse_slash("/title Migration Plan").unwrap();
    assert_eq!(name, "title");
    commands
        .get("title")
        .unwrap()
        .unwrap()
        .execute(&agent, &args)
        .await
        .unwrap();

    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        heycode_agent::title::current(s.events()).as_deref(),
        Some("Migration Plan")
    );
}

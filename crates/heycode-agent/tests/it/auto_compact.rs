//! Auto-compaction: pressure crossing folds history before the next step.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_agent::{
    AutoApprove, CompactionPolicy, agent_plugin, approval_plugin, commands_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, LlmSelection, Provider, ProviderInfo, StreamChunk, llm_plugin,
    model_catalog_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_session::{Session, SessionEventKind, session_plugin};
use heycode_tools::tools_plugin;

#[derive(Default)]
struct RecordingSettingsWriter {
    writes: Mutex<Vec<serde_json::Value>>,
}

impl heycode_settings::SettingsWriter for RecordingSettingsWriter {
    fn persist_user(
        &self,
        _namespace: &heycode_settings::SettingsNamespace,
        section: &serde_json::Value,
    ) -> Result<(), String> {
        self.writes.lock().unwrap().push(section.clone());
        Ok(())
    }
}

fn writable_settings_plugin(writer: Arc<RecordingSettingsWriter>) -> Box<dyn Plugin> {
    struct WritableSettings(Arc<RecordingSettingsWriter>);
    impl Plugin for WritableSettings {
        fn name(&self) -> &'static str {
            "test-writable-settings"
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }
        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            context.provide(
                heycode_settings::SERVICE_SETTINGS,
                self.name(),
                heycode_settings::SettingsService::with_writer(
                    heycode_settings::SettingsDocuments::new(),
                    self.0.clone() as Arc<dyn heycode_settings::SettingsWriter>,
                ),
            )
        }
    }
    Box::new(WritableSettings(writer))
}

fn execution_plugin() -> Box<dyn heycode_core::Plugin> {
    heycode_exec::local_execution_plugin(
        heycode_exec::LocalShellConfig::platform(
            std::env::current_dir().unwrap(),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
    )
}

struct Recording {
    inner: FakeProvider,
    sink: Arc<Mutex<Vec<ChatRequest>>>,
}

impl Provider for Recording {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.sink.lock().unwrap().push(request.clone());
        self.inner.stream(request)
    }
}

fn stop_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

fn long_text(tokens_worth: usize) -> Vec<StreamChunk> {
    // ~4 chars per token estimate in the agent.
    let body = "x".repeat(tokens_worth * 4);
    vec![
        StreamChunk::TextDelta(body),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

#[tokio::test]
async fn pressure_crossing_triggers_auto_compaction_before_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(vec![
            long_text(200),             // turn 1 reply ≈ 200 tokens
            stop_text("second answer"), // turn 2 reply (short)
            stop_text("AUTO-SUMMARY"),  // consumed by auto-compaction on turn 3
            stop_text("third answer"),  // turn 3 reply
            stop_text("fourth answer"), // turn 4 reply
        ]),
        sink: requests.clone(),
    });

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
        // Low pressure threshold with enough hard capacity for the full tool catalog.
        options_plugin(CompactionPolicy {
            auto: true,
            threshold_ratio: 0.005,
            context_window: 20_000,
        }),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    agent.send("one").await.unwrap();
    agent.send("two").await.unwrap();
    agent.send("three").await.unwrap();
    agent.send("four").await.unwrap(); // pre-step here crosses pressure and folds turn 1

    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        s.events()
            .iter()
            .any(|e| matches!(e.kind, SessionEventKind::CompactionApplied { .. })),
        "auto compaction must have run"
    );
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 5, "t1 + t2 + t3 + summarizer + rebuilt t4");
    // The rebuilt request carries the summary right after the system prompt.
    assert!(
        reqs[4]
            .messages
            .iter()
            .any(|m| m.content.contains("<compacted-summary>")),
        "rebuilt request must lead with the summary"
    );
    assert!(
        !reqs[3]
            .messages
            .iter()
            .any(|m| m.content.contains("<compacted-summary>"))
    );
}

#[tokio::test]
async fn disabled_policy_never_compacts() {
    let dir = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(vec![long_text(200), long_text(200)]),
        sink: requests.clone(),
    });
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
        options_plugin(CompactionPolicy {
            auto: false,
            threshold_ratio: 0.005,
            context_window: 20_000,
        }),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("a").await.unwrap();
    agent.send("b").await.unwrap();

    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        !s.events()
            .iter()
            .any(|e| matches!(e.kind, SessionEventKind::CompactionApplied { .. }))
    );
}

#[tokio::test]
async fn autocompact_command_persists_and_updates_the_live_threshold_control() {
    let dir = tempfile::tempdir().unwrap();
    let writer = Arc::new(RecordingSettingsWriter::default());
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![]));
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
        writable_settings_plugin(writer.clone()),
        options_plugin(CompactionPolicy {
            auto: true,
            threshold_ratio: 0.8,
            context_window: 1_000_000,
        }),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
        heycode_agent::autocompact_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let command = commands.get("autocompact").unwrap().unwrap();
    assert!(command.availability().is_available());
    assert_eq!(command.descriptor().synopsis(), "/autocompact [threshold]");
    assert_eq!(agent.auto_compaction_control().fixed_tokens(), None);

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    command.execute(&agent, "").await.unwrap();
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::AutoCompactPickerRequested {
            enabled: true,
            current_tokens: None
        }
    )));
    assert!(writer.writes.lock().unwrap().is_empty());

    command.execute(&agent, "500k").await.unwrap();
    assert_eq!(
        agent.auto_compaction_control().fixed_tokens(),
        Some(500_000)
    );
    assert_eq!(writer.writes.lock().unwrap()[0]["mode"], "fixed");
    assert_eq!(writer.writes.lock().unwrap()[0]["tokens"], 500_000);
    assert!(command.execute(&agent, "99k").await.is_err());
    assert_eq!(writer.writes.lock().unwrap().len(), 1);

    command.execute(&agent, "auto").await.unwrap();
    assert_eq!(agent.auto_compaction_control().fixed_tokens(), None);
    assert_eq!(writer.writes.lock().unwrap()[1]["mode"], "auto");
    context.shutdown();
}

/// Publish `AgentOptions` with a custom compaction policy.
fn options_plugin(policy: CompactionPolicy) -> Box<dyn Plugin> {
    struct Opts(heycode_agent::AgentOptions);
    impl Plugin for Opts {
        fn name(&self) -> &'static str {
            "agent-options"
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
            ctx.provide(
                heycode_agent::SERVICE_AGENT_OPTIONS,
                "agent-options",
                heycode_agent::AgentOptions {
                    compaction: self.0.compaction,
                    max_task_depth: 3,
                    auto_title: false,
                    cwd: None,
                },
            )
        }
    }
    Box::new(Opts(heycode_agent::AgentOptions {
        compaction: policy,
        max_task_depth: 3,
        auto_title: false,
        cwd: None,
    }))
}

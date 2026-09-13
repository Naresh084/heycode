//! `/compact` end-to-end: real turn, then fold, then projection proof.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_agent::{
    AgentOptions, AutoApprove, CommandTiming, CompactionError, PortableCompaction, PruneCompaction,
    agent_options_plugin, agent_plugin, approval_plugin, commands_plugin, parse_slash,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, LlmSelection, Provider, ProviderInfo, StreamChunk, llm_plugin,
    model_catalog_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_session::{Session, SessionEventKind, TurnEndReason, session_plugin};
use heycode_tools::tools_plugin;
use tokio_util::sync::CancellationToken;

use super::turn::{World, build_provider_in};

const SUMMARY_SYSTEM: &str = "Summarize the conversation for a coding assistant continuing the work. Preserve the user's goals, decisions, files touched, and unfinished steps. Be concise and factual.";
const MAX_SUMMARIZE_INPUT_BYTES: usize = 24_000;

#[derive(Clone, Copy)]
enum SummaryMode {
    Complete,
    Incomplete,
    Hanging,
}

struct SummaryProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    mode: SummaryMode,
    entered: Option<Arc<tokio::sync::Notify>>,
}

impl Provider for SummaryProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "fake".to_owned(),
            default_model: "test-model".to_owned(),
        }
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        let is_summary = request
            .messages
            .first()
            .is_some_and(|message| message.content.starts_with(SUMMARY_SYSTEM));
        self.requests.lock().unwrap().push(request);
        if !is_summary {
            return Box::pin(futures::stream::iter(
                script_text("CONTINUED").into_iter().map(Ok),
            ));
        }
        if let Some(entered) = &self.entered {
            entered.notify_one();
        }
        match self.mode {
            SummaryMode::Complete => Box::pin(futures::stream::iter(
                script_text("FOCUSED SUMMARY preserves the required details")
                    .into_iter()
                    .map(Ok),
            )),
            SummaryMode::Incomplete => Box::pin(futures::stream::iter([Ok(
                StreamChunk::TextDelta("partial summary".to_owned()),
            )])),
            SummaryMode::Hanging => Box::pin(futures::stream::pending()),
        }
    }
}

fn summary_world(mode: SummaryMode, entered: Option<Arc<tokio::sync::Notify>>) -> World {
    build_provider_in(
        tempfile::tempdir().unwrap(),
        move |requests| {
            Arc::new(SummaryProvider {
                requests,
                mode,
                entered,
            })
        },
        Arc::new(AutoApprove),
    )
}

fn append_turn(session: &mut Session, turn: u64, user: &str, assistant: &str) {
    session
        .append(SessionEventKind::UserMessage {
            text: user.to_owned(),
        })
        .unwrap();
    session
        .append(SessionEventKind::TurnStart { turn })
        .unwrap();
    session
        .append(SessionEventKind::AssistantMessage {
            turn,
            step: 0,
            content: assistant.to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: None,
        })
        .unwrap();
    session
        .append(SessionEventKind::TurnEnd {
            turn,
            reason: TurnEndReason::Stop,
        })
        .unwrap();
}

fn session_bytes(world: &World) -> Vec<u8> {
    let path = world.session.lock().unwrap().path().to_path_buf();
    std::fs::read(path).unwrap()
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

fn script_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

#[tokio::test]
async fn compact_folds_history_and_projection_carries_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = vec![
        script_text("answer one"),
        script_text("answer two"),
        // the summarizer call consumes this third script
        script_text("COMPACT SUMMARY: user asked for one; assistant answered."),
    ];
    let provider = Arc::new(FakeProvider::new(scripts));
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

    agent.send("question one").await.unwrap();
    agent.send("question two").await.unwrap();

    // /compact keep=1 → folds everything except the last turn.
    let (name, args) = parse_slash("/compact 1").unwrap();
    assert_eq!(name, "compact");
    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let cmd = commands.get("compact").unwrap().unwrap();
    cmd.execute(&agent, &args).await.unwrap();

    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    {
        let s = session.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            s.events()
                .iter()
                .any(|e| matches!(e.kind, SessionEventKind::CompactionApplied { .. }))
        );
    }

    // Projection: summary first (as User), then the kept recent turn.
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    let wires = heycode_session::derive_messages(s.events());
    assert_eq!(wires.len(), 3, "summary + kept user/assistant pair");
    assert_eq!(wires[0].role, heycode_session::Role::User);
    assert!(wires[0].content.contains("<compacted-summary>"));
    assert!(wires[0].content.contains("COMPACT SUMMARY"));
    assert_eq!(wires[1].content, "question two");
}

#[tokio::test]
async fn compact_without_enough_history_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![script_text("only")]));
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
    agent.send("hi").await.unwrap();

    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let compact = commands.get("compact").unwrap().unwrap();
    assert_eq!(compact.descriptor().arguments().len(), 3);
    compact.execute(&agent, "").await.unwrap();

    let ui = Arc::new(Mutex::new(Vec::new()));
    let captured = ui.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        captured.lock().unwrap().push(event.clone());
    });
    compact.execute(&agent, "list").await.unwrap();
    let listing = ui.lock().unwrap().iter().find_map(|event| match event {
        heycode_agent::UiEvent::Info { text } if text.starts_with("compaction strategies") => {
            Some(text.clone())
        }
        _ => None,
    });
    let listing = listing.expect("strategy listing");
    for strategy in ["portable-summary", "provider-native", "prune-oldest"] {
        assert!(listing.contains(strategy), "{listing}");
    }

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
async fn no_focus_keeps_the_original_summary_request_exactly() {
    let world = summary_world(SummaryMode::Complete, None);
    {
        let mut session = world.session.lock().unwrap();
        append_turn(&mut session, 0, "old question", "old answer");
        append_turn(&mut session, 1, "recent question", "recent answer");
    }

    world
        .agent
        .compact(PortableCompaction::ID, 1, CancellationToken::new())
        .await
        .unwrap();

    let requests = world.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model, "test-model");
    assert!(request.tools.is_none());
    assert_eq!(request.max_tokens, Some(2_048));
    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.messages[0].role, heycode_llm::Role::System);
    assert_eq!(request.messages[0].content, SUMMARY_SYSTEM);
    assert_eq!(request.messages[1].role, heycode_llm::Role::User);
    assert_eq!(
        request.messages[1].content,
        "User: old question\nAssistant: old answer\n"
    );
}

#[tokio::test]
async fn focused_hierarchy_repeats_focus_within_budget_and_continues_from_projection() {
    let world = summary_world(SummaryMode::Complete, None);
    let old = format!("OLD-FOLDED-SENTINEL {}", "x".repeat(70_000));
    {
        let mut session = world.session.lock().unwrap();
        append_turn(&mut session, 0, &old, "old assistant answer");
        append_turn(
            &mut session,
            1,
            "RECENT-KEPT-QUESTION",
            "RECENT-KEPT-ANSWER",
        );
    }
    let original = session_bytes(&world);
    let focus = "Prioritize API decisions, cancellation, and exact filenames.";

    world
        .agent
        .compact_with_focus(
            PortableCompaction::ID,
            1,
            Some(focus),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let after_compaction = session_bytes(&world);
    assert!(after_compaction.starts_with(&original));
    {
        let requests = world.requests.lock().unwrap();
        assert!(
            requests.len() >= 4,
            "hierarchical summary calls: {}",
            requests.len()
        );
        for request in requests.iter() {
            assert_eq!(request.messages.len(), 2);
            let system = &request.messages[0].content;
            let user = &request.messages[1].content;
            assert!(system.starts_with(SUMMARY_SYSTEM));
            assert!(system.contains("without inventing facts"));
            assert!(user.starts_with("Explicit human compaction focus (JSON string): "));
            assert!(user.contains(focus));
            assert!(user.contains("Conversation to summarize:"));
            assert!(
                system.len() + user.len() <= SUMMARY_SYSTEM.len() + MAX_SUMMARIZE_INPUT_BYTES,
                "focused request exceeded the unchanged no-focus input envelope: {}",
                system.len() + user.len()
            );
        }
    }

    let projection = {
        let session = world.session.lock().unwrap();
        heycode_session::derive_messages(session.events())
    };
    let projected = projection
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!projected.contains("OLD-FOLDED-SENTINEL"));
    assert!(projected.contains("FOCUSED SUMMARY"));
    assert!(projected.contains("RECENT-KEPT-QUESTION"));

    world
        .agent
        .send("continue after focused compact")
        .await
        .unwrap();
    let requests = world.requests.lock().unwrap();
    let continuation = requests.last().unwrap();
    let wire = continuation
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!wire.contains("OLD-FOLDED-SENTINEL"));
    assert!(wire.contains("FOCUSED SUMMARY"));
    assert!(wire.contains("RECENT-KEPT-QUESTION"));
    assert!(session_bytes(&world).starts_with(&original));
}

#[tokio::test]
async fn focused_failure_and_cancellation_leave_the_log_byte_exact() {
    let failed = summary_world(SummaryMode::Incomplete, None);
    {
        let mut session = failed.session.lock().unwrap();
        append_turn(&mut session, 0, "old", "answer");
        append_turn(&mut session, 1, "recent", "answer");
    }
    let before_failure = session_bytes(&failed);
    let error = failed
        .agent
        .compact_with_focus(
            PortableCompaction::ID,
            1,
            Some("focus"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ended before finish"));
    assert_eq!(session_bytes(&failed), before_failure);

    let entered = Arc::new(tokio::sync::Notify::new());
    let cancelled = summary_world(SummaryMode::Hanging, Some(entered.clone()));
    {
        let mut session = cancelled.session.lock().unwrap();
        append_turn(&mut session, 0, "old", "answer");
        append_turn(&mut session, 1, "recent", "answer");
    }
    let before_cancellation = session_bytes(&cancelled);
    let token = CancellationToken::new();
    let task_token = token.clone();
    let agent = cancelled.agent.clone();
    let task = tokio::spawn(async move {
        agent
            .compact_with_focus(PortableCompaction::ID, 1, Some("focus"), task_token)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    token.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error, CompactionError::Cancelled);
    assert_eq!(session_bytes(&cancelled), before_cancellation);
}

#[tokio::test]
async fn compact_command_propagates_cancellation_without_false_ui_settlement() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let world = summary_world(SummaryMode::Hanging, Some(entered.clone()));
    {
        let mut session = world.session.lock().unwrap();
        append_turn(&mut session, 0, "old", "answer");
        append_turn(&mut session, 1, "recent", "answer");
    }
    let before = session_bytes(&world);
    let command = world
        .ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("compact")
        .unwrap()
        .unwrap();
    assert_eq!(
        command.descriptor().timing(),
        CommandTiming::ModelScheduling
    );

    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let agent = world.agent.clone();
    let task = tokio::spawn(async move {
        command
            .execute_cancellable(&agent, "1 -- focus", task_cancellation)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    cancellation.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<CompactionError>(),
        Some(&CompactionError::Cancelled)
    );
    assert_eq!(session_bytes(&world), before);
    assert!(world.ui_log.lock().unwrap().iter().all(|event| !matches!(
        event,
        heycode_agent::UiEvent::Info { text } if text.contains("folded")
    ) && !matches!(
        event,
        heycode_agent::UiEvent::Error { message } if message.contains("cancelled")
    )));
}

#[tokio::test]
async fn invalid_or_unsupported_focus_is_rejected_before_provider_or_commit() {
    let world = summary_world(SummaryMode::Complete, None);
    {
        let mut session = world.session.lock().unwrap();
        append_turn(&mut session, 0, "old", "answer");
        append_turn(&mut session, 1, "recent", "answer");
    }
    let before = session_bytes(&world);

    for focus in ["   ".to_owned(), "x".repeat(4 * 1024 + 1)] {
        let error = world
            .agent
            .compact_with_focus(
                PortableCompaction::ID,
                1,
                Some(&focus),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, CompactionError::InvalidFocus);
    }
    let error = world
        .agent
        .compact_with_focus(
            PruneCompaction::ID,
            1,
            Some("focus"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, CompactionError::FocusUnsupported(_)));
    assert!(world.requests.lock().unwrap().is_empty());
    assert_eq!(session_bytes(&world), before);
}

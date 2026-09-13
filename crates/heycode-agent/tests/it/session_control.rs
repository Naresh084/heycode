//! Shipping native command/question/checkpoint behavior, with deterministic providers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::turn::{build, script_text};
use heycode_agent::{Agent, AgentOptions, AutoApprove, CommandRegistry, OutputStyle};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{ChatRequest, ChunkStream, FinishReason, Provider, ProviderInfo, StreamChunk};
use heycode_session::Session;
use std::sync::{Arc, Mutex};

async fn command(world: &super::turn::World, name: &str, args: &str) -> anyhow::Result<()> {
    world
        .ctx
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get(name)?
        .unwrap()
        .execute(&world.agent, args)
        .await
}

#[tokio::test]
async fn side_question_is_tool_free_and_does_not_touch_main_log_or_turn() {
    let world = build(
        vec![script_text("main answer"), script_text("side answer")],
        Arc::new(AutoApprove),
    );
    world.agent.send("main task").await.unwrap();
    let before = serde_json::to_value(world.session.lock().unwrap().events()).unwrap();
    assert_eq!(
        world.agent.side_question("What did we do?").await.unwrap(),
        "side answer"
    );
    assert_eq!(
        before,
        serde_json::to_value(world.session.lock().unwrap().events()).unwrap()
    );
    assert!(!world.agent.token().is_turn_active());
    let requests = world.requests.lock().unwrap();
    let aside = requests.last().unwrap();
    assert!(aside.tools.is_none());
    assert_eq!(aside.max_tokens, Some(2048));
    assert!(aside.messages[1].content.contains("main task"));
    assert!(
        aside.messages[1].content.contains("side question")
            || aside.messages[1].content.contains("Side question")
    );
}

#[tokio::test]
async fn style_and_recap_preferences_persist_without_model_messages_and_apply_to_requests() {
    let world = build(vec![script_text("styled answer")], Arc::new(AutoApprove));
    let events = world.session.lock().unwrap().events().len();
    command(&world, "output-style", "concise").await.unwrap();
    command(&world, "recap", "off").await.unwrap();
    assert!(matches!(
        world.agent.output_style().unwrap(),
        OutputStyle::Concise
    ));
    assert!(!world.agent.automatic_recap_enabled().unwrap());
    assert_eq!(events, world.session.lock().unwrap().events().len());
    tokio::time::timeout(std::time::Duration::from_secs(3), world.agent.send("hello"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        world.requests.lock().unwrap()[0].messages[0]
            .content
            .contains("Be concise.")
    );
    assert!(command(&world, "output-style", "custom").await.is_err());
    assert!(matches!(
        world.agent.output_style().unwrap(),
        OutputStyle::Concise
    ));
    let path = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .join("session-controls.json");
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(state["auto_recap"], false);
    assert_eq!(state["style"], "concise");
}

#[tokio::test]
async fn asynchronous_questions_return_before_answer_and_commit_exactly_once() {
    let world = build(vec![script_text("after answer")], Arc::new(AutoApprove));
    let tools = world
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tool = tools.get("ask_user_question_async").unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tool.run(
            serde_json::json!({"question":"Which format?","options":["Markdown","Text"]}),
            &heycode_tools::ToolCtx::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let id = result["question_id"].as_str().unwrap();
    assert_eq!(world.agent.async_questions().unwrap().len(), 1);
    assert!(world.agent.pending_inbox().is_empty());
    let state_path = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .join("session-controls.json");
    let before_cleanup = std::fs::read(&state_path).unwrap();
    world.agent.answer_async(id, "Markdown").unwrap();
    assert!(
        matches!(world.session.lock().unwrap().inbox().next_turn()[0].source(), heycode_session::InboxSource::OptionalQuestion { question_id, .. } if question_id.as_str() == id)
    );
    // Simulate a crash after authoritative inbox commit but before sidecar cleanup.
    std::fs::write(&state_path, before_cleanup).unwrap();
    assert!(world.agent.answer_async(id, "Text").is_err());
    assert!(world.agent.async_questions().unwrap().is_empty());
    assert_eq!(world.agent.pending_inbox().next_turn, 1);
    world
        .agent
        .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let messages = heycode_session::derive_messages(world.session.lock().unwrap().events());
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.content.contains("Answer to optional question"))
            .count(),
        1
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("Which format?")
                && message.content.contains("Markdown"))
    );
}

#[tokio::test]
async fn pending_questions_survive_reopen_and_cancellation_never_invents_answer() {
    let world = build(Vec::new(), Arc::new(AutoApprove));
    let tools = world
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tool = tools.get("ask_user_question_async").unwrap();
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert!(
        tool.run(
            serde_json::json!({"question":"Do we proceed?"}),
            &heycode_tools::ToolCtx {
                cancellation: cancelled,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert!(world.agent.async_questions().unwrap().is_empty());
    let result = tool
        .run(
            serde_json::json!({"question":"Which file?"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    let id = result["question_id"].as_str().unwrap();
    let path = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    let reopened = Session::open(&path).unwrap();
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("session-controls.json")).unwrap())
            .unwrap();
    assert_eq!(stored["questions"][0]["id"], id);
    assert!(reopened.inbox().next_turn().is_empty());
    command(&world, "questions", &format!("cancel {id}"))
        .await
        .unwrap();
    assert!(world.agent.async_questions().unwrap().is_empty());
    assert!(world.agent.pending_inbox().is_empty());
}

#[tokio::test]
async fn rewind_forks_before_exact_prompt_and_preserves_original_log() {
    let world = build(
        vec![script_text("one"), script_text("two")],
        Arc::new(AutoApprove),
    );
    world.agent.send("first prompt").await.unwrap();
    world.agent.send("second prompt").await.unwrap();
    let points = world.agent.rewind_points();
    assert_eq!(points.len(), 2);
    assert_eq!(points[1].prompt, "second prompt");
    let before = serde_json::to_value(world.session.lock().unwrap().events()).unwrap();
    let child_id = world.agent.rewind(points[1].turn, false).await.unwrap();
    let root = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let child = Session::open(root.join(child_id.as_str())).unwrap();
    let messages = heycode_session::derive_messages(child.events());
    assert!(messages.iter().any(|m| m.content == "first prompt"));
    assert!(!messages.iter().any(|m| m.content == "second prompt"));
    assert_eq!(
        before,
        serde_json::to_value(world.session.lock().unwrap().events()).unwrap()
    );
    world
        .agent
        .submit_inbox(heycode_session::InboxDelivery::FollowUp, "pending")
        .unwrap();
    assert!(world.agent.rewind(points[0].turn, false).await.is_err());
}

#[tokio::test]
async fn rewind_of_follow_up_does_not_resurrect_excluded_inbox_message() {
    let world = build(
        vec![script_text("one"), script_text("two")],
        Arc::new(AutoApprove),
    );
    world.agent.send("first prompt").await.unwrap();
    world
        .agent
        .submit_inbox(heycode_session::InboxDelivery::FollowUp, "queued prompt")
        .unwrap();
    world
        .agent
        .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let point = world.agent.rewind_points().pop().unwrap();
    assert_eq!(point.prompt, "queued prompt");
    let child_id = world.agent.rewind(point.turn, false).await.unwrap();
    let root = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let child = Session::open(root.join(child_id.as_str())).unwrap();
    assert!(child.inbox().next_turn().is_empty());
    assert!(
        !heycode_session::derive_messages(child.events())
            .iter()
            .any(|m| m.content == "queued prompt")
    );
}

struct Recording {
    inner: FakeProvider,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}
impl Provider for Recording {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.stream(request)
    }
}

#[tokio::test]
async fn native_write_checkpoint_is_restorable_and_user_edit_conflicts_are_non_destructive() {
    exercise_native_rewind(false).await;
}

#[tokio::test]
async fn run_code_file_edits_are_checkpointed_and_conflicts_preserve_user_content() {
    exercise_native_rewind(true).await;
}

async fn exercise_native_rewind(code_mode: bool) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tool_script = vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some("write-one".into()),
            name: Some("write".into()),
            arguments_delta: serde_json::json!({"path":"created.txt","content":"agent content"})
                .to_string(),
        },
        StreamChunk::Finish(FinishReason::ToolCalls),
    ];
    let tool_script = if code_mode {
        vec![
            StreamChunk::ToolCallDelta {
                index: 0,
                id: Some("script-edits".into()),
                name: Some("run_code".into()),
                arguments_delta: serde_json::json!({
                    "source": "await tools.write({path:'created.txt',content:'initial content'}); return await tools.edit({path:'created.txt',old_string:'initial',new_string:'agent'});",
                    "tools": ["write", "edit"]
                }).to_string(),
            },
            StreamChunk::Finish(FinishReason::ToolCalls),
        ]
    } else {
        tool_script
    };
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(root.path().join("sessions")),
        heycode_prompt::prompt_plugin(),
        super::turn::execution_plugin(&workspace),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_tools::tools_plugin(Default::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            heycode_llm::LlmSelection {
                provider_name: "fake".into(),
                model: "test-model".into(),
            },
            vec![Arc::new(Recording {
                inner: FakeProvider::new(vec![tool_script, script_text("created")]),
                requests,
            })],
        ),
        heycode_agent::approval_plugin(Arc::new(AutoApprove)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::agent_options_plugin(AgentOptions {
            cwd: Some(workspace.clone()),
            ..Default::default()
        }),
        heycode_agent::agent_plugin(),
    ];
    let mut ctx = compose(&plugins).unwrap();
    heycode_agent::code_mode_plugin().apply(&mut ctx).unwrap();
    let agent = ctx.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    agent.send("create a file").await.unwrap();
    let point = agent.rewind_points().pop().unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("created.txt")).unwrap(),
        "agent content"
    );
    std::fs::write(workspace.join("created.txt"), "user edit").unwrap();
    assert!(agent.rewind(point.turn, true).await.is_err());
    assert_eq!(
        std::fs::read_to_string(workspace.join("created.txt")).unwrap(),
        "user edit"
    );
    std::fs::write(workspace.join("created.txt"), "agent content").unwrap();
    agent.rewind(point.turn, true).await.unwrap();
    assert!(!workspace.join("created.txt").exists());
    assert!(std::fs::read_dir(&workspace).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".heycode-rewind-")
    }));
    ctx.shutdown();
}

struct HangThenAside {
    calls: std::sync::atomic::AtomicUsize,
    started: tokio::sync::Notify,
}

struct CancelThenRecap {
    calls: std::sync::atomic::AtomicUsize,
    started: tokio::sync::Notify,
    first_stream_dropped: Arc<std::sync::atomic::AtomicBool>,
}

struct DropObservedPending {
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

impl futures::Stream for DropObservedPending {
    type Item = Result<StreamChunk, heycode_llm::LlmError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Pending
    }
}

impl Drop for DropObservedPending {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Provider for CancelThenRecap {
    fn info(&self) -> ProviderInfo {
        FakeProvider::new(Vec::new()).info()
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            self.started.notify_one();
            Box::pin(DropObservedPending {
                dropped: self.first_stream_dropped.clone(),
            })
        } else {
            Box::pin(futures::stream::iter(
                script_text("retry recap").into_iter().map(Ok),
            ))
        }
    }
}

impl Provider for HangThenAside {
    fn info(&self) -> ProviderInfo {
        FakeProvider::new(Vec::new()).info()
    }
    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        use futures::StreamExt as _;
        self.started.notify_one();
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Box::pin(
                futures::stream::iter([Ok(StreamChunk::TextDelta("working".into()))])
                    .chain(futures::stream::pending()),
            )
        } else {
            Box::pin(futures::stream::iter(
                script_text("aside while working").into_iter().map(Ok),
            ))
        }
    }
}

#[tokio::test]
async fn recap_command_propagates_cancellation_and_never_emits_a_stale_receipt() {
    let first_stream_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = Arc::new(CancelThenRecap {
        calls: std::sync::atomic::AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        first_stream_dropped: first_stream_dropped.clone(),
    });
    let world = super::turn::build_provider_in(
        tempfile::tempdir().unwrap(),
        |_| provider.clone(),
        Arc::new(AutoApprove),
    );
    let before = serde_json::to_value(world.session.lock().unwrap().events()).unwrap();
    let recap = world
        .ctx
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("recap")
        .unwrap()
        .unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let agent = world.agent.clone();
    let task = tokio::spawn(async move {
        recap
            .execute_cancellable(&agent, "", task_cancellation)
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.started.notified(),
    )
    .await
    .unwrap();
    cancellation.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("cancelled recap did not settle")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("side question cancelled"));
    assert!(first_stream_dropped.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        before,
        serde_json::to_value(world.session.lock().unwrap().events()).unwrap()
    );
    assert!(!world.ui_log.lock().unwrap().iter().any(
        |event| matches!(event, heycode_agent::UiEvent::Info { text } if text.starts_with("Recap:"))
    ));

    command(&world, "recap", "").await.unwrap();
    assert!(world.ui_log.lock().unwrap().iter().any(
        |event| matches!(event, heycode_agent::UiEvent::Info { text } if text == "Recap: retry recap")
    ));
}

#[tokio::test]
async fn side_question_runs_while_main_turn_is_active_without_cancelling_it() {
    let provider = Arc::new(HangThenAside {
        calls: std::sync::atomic::AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
    });
    let world = super::turn::build_provider_in(
        tempfile::tempdir().unwrap(),
        |_| provider.clone(),
        Arc::new(AutoApprove),
    );
    let agent = world.agent.clone();
    let running = tokio::spawn(async move { agent.send("main work").await });
    provider.started.notified().await;
    assert!(world.agent.token().is_turn_active());
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            world.agent.side_question("brief aside")
        )
        .await
        .unwrap()
        .unwrap(),
        "aside while working"
    );
    assert!(world.agent.token().is_turn_active());
    assert!(!world.agent.token().is_cancelled());
    world.agent.token().cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn interrupted_side_request_fails_without_inventing_a_completed_answer() {
    let world = build(
        vec![vec![StreamChunk::TextDelta("partial".into())]],
        Arc::new(AutoApprove),
    );
    assert!(world.agent.side_question("question").await.is_err());
    assert!(heycode_session::derive_messages(world.session.lock().unwrap().events()).is_empty());
}

#[tokio::test]
async fn conversation_rewind_survives_corrupt_file_journal_and_restores_prompt_draft() {
    let world = build(vec![script_text("answer")], Arc::new(AutoApprove));
    world.agent.send("original prompt").await.unwrap();
    let directory = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir(directory.join("checkpoints")).unwrap();
    std::fs::write(
        directory.join("checkpoints/12345678-1234-1234-1234-123456789abc.json"),
        b"damaged",
    )
    .unwrap();
    let point = world.agent.rewind_points().pop().unwrap();
    assert!(world.agent.rewind(point.turn, true).await.is_err());
    let child = world.agent.rewind(point.turn, false).await.unwrap();
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            directory
                .parent()
                .unwrap()
                .join(child.as_str())
                .join("session-controls.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["rewind_draft"], "original prompt");
    assert_eq!(state["file_rewind_floor"], point.event_count);
    assert!(
        world
            .agent
            .rewind_points()
            .iter()
            .any(|point| point.prompt == "original prompt")
    );
}

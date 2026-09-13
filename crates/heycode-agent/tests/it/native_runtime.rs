//! A01 existing native loop through the R01 runtime contract/registry.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, AutoApprove, agent_options_plugin, agent_plugin, approval_plugin,
    commands_plugin, native_runtime_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, LlmSelection, Provider, ProviderInfo, StreamChunk, llm_plugin,
    model_catalog_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_runtime::{
    AccountStatus, AgentRuntimeKind, RuntimeErrorCode, RuntimeEventKind, RuntimeFinishReason,
    RuntimeFork, RuntimeInput, RuntimeResume, RuntimeSession, RuntimeStart,
    runtime_registry_plugin,
};
use heycode_session::{
    SessionCreationMetadata, SessionEventKind, SessionSource, TurnEndReason,
    session_with_metadata_plugin,
};
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
use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;

struct HangThenReply {
    calls: std::sync::atomic::AtomicUsize,
}

impl Provider for HangThenReply {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "fake".to_owned(),
            default_model: "test-model".to_owned(),
        }
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
            return Box::pin(futures::stream::iter([
                Ok(StreamChunk::TextDelta("recovered".to_owned())),
                Ok(StreamChunk::Finish(heycode_llm::FinishReason::Stop)),
            ]));
        }
        Box::pin(futures::stream::unfold(0_u8, |state| async move {
            match state {
                0 => Some((Ok(StreamChunk::TextDelta("partial".to_owned())), 1)),
                _ => std::future::pending().await,
            }
        }))
    }
}

struct World {
    context: heycode_core::Context,
    workspace: tempfile::TempDir,
    _sessions: tempfile::TempDir,
}

fn world(provider: Arc<dyn Provider>) -> World {
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_with_metadata_plugin(
            sessions.path().to_path_buf(),
            SessionCreationMetadata::new(
                Some(workspace.path().to_path_buf()),
                Some("native".to_owned()),
                SessionSource::Interactive,
            )
            .unwrap(),
        ),
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
                provider_name: "fake".to_owned(),
                model: "test-model".to_owned(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        agent_options_plugin(AgentOptions {
            cwd: Some(workspace.path().to_path_buf()),
            ..AgentOptions::default()
        }),
        runtime_registry_plugin(),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
        native_runtime_plugin(),
    ];
    World {
        context: compose(&plugins).unwrap(),
        workspace,
        _sessions: sessions,
    }
}

fn session_id(world: &World) -> heycode_core::SessionId {
    world
        .context
        .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
        .unwrap()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .id()
        .clone()
}

async fn start_native(world: &World) -> Arc<dyn RuntimeSession> {
    let registry = world
        .context
        .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = registry.get("native").unwrap().unwrap();
    runtime
        .start(
            RuntimeStart::new(session_id(world), world.workspace.path()).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn projected_context_uses_retained_text_not_billed_completion_or_hidden_reasoning() {
    let provider = Arc::new(FakeProvider::new(vec![vec![
        StreamChunk::ReasoningDelta("hidden reasoning ".repeat(1000)),
        StreamChunk::TextDelta("hello".to_owned()),
        StreamChunk::Usage(heycode_core::TokenUsage {
            prompt_tokens: 3,
            completion_tokens: 50_000,
        }),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]]));
    let world = world(provider);
    let runtime = start_native(&world).await;
    let mut events = runtime.subscribe();
    runtime
        .send(
            RuntimeInput::new("say hello").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut measured = None;
    let mut projected = None;
    let mut billed = None;
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match event.kind() {
            RuntimeEventKind::ContextBudgetChanged { budget } if budget.projected => {
                projected = Some(budget.clone())
            }
            RuntimeEventKind::ContextBudgetChanged { budget } => measured = Some(budget.clone()),
            RuntimeEventKind::Usage { usage, .. } => billed = Some(usage.completion_tokens),
            RuntimeEventKind::TurnFinished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(billed, Some(50_000), "billing usage remains unchanged");
    let projected = projected.unwrap();
    assert_eq!(
        projected.used - measured.unwrap().used,
        2,
        "only retained canonical text contributes to this estimated growth"
    );
    let agent = world
        .context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let (bus, duplicate) = {
        let session = agent.session().lock().unwrap();
        let event = session
            .events()
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.kind,
                    heycode_session::SessionEventKind::AssistantMessage { .. }
                )
            })
            .unwrap()
            .clone();
        (session.bus(), event)
    };
    bus.emit(duplicate);
    assert_eq!(
        agent.context_budget().unwrap().used,
        projected.used,
        "duplicate durable event delivery must not inflate projected context"
    );
}

#[tokio::test]
async fn native_runtime_registry_send_and_events_preserve_the_existing_durable_turn() {
    let provider = Arc::new(FakeProvider::new(vec![vec![
        StreamChunk::TextDelta("hello".to_owned()),
        StreamChunk::Usage(heycode_core::TokenUsage {
            prompt_tokens: 3,
            completion_tokens: 2,
        }),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]]));
    let mut world = world(provider);
    let registry = world
        .context
        .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let descriptors = registry.descriptors().unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id().as_str(), "native");
    assert_eq!(descriptors[0].kind(), AgentRuntimeKind::Native);
    assert!(descriptors[0].capabilities().models.is_supported());
    let runtime = registry.get("native").unwrap().unwrap();
    assert_eq!(
        runtime
            .account(CancellationToken::new())
            .await
            .unwrap()
            .status(),
        AccountStatus::Unknown
    );
    assert_eq!(
        runtime
            .models(CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Unavailable,
        "fake provider deliberately has no catalog source"
    );

    let session = start_native(&world).await;
    assert_eq!(session.id().as_str(), session_id(&world).as_str());
    let mut events = session.subscribe();
    let turn = session
        .send(
            RuntimeInput::new("say hello").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(turn.as_str(), "1");

    let mut kinds = Vec::new();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let terminal = matches!(event.kind(), RuntimeEventKind::TurnFinished { .. });
        kinds.push(event.kind().clone());
        if terminal {
            break;
        }
    }
    assert!(matches!(kinds[0], RuntimeEventKind::SessionReady));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        RuntimeEventKind::CommentaryDelta { text } if text == "hello"
    )));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        RuntimeEventKind::FinalMessage { text } if text == "hello"
    )));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        RuntimeEventKind::Usage { usage, .. }
            if usage.prompt_tokens == 3 && usage.completion_tokens == 2
    )));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        RuntimeEventKind::TurnFinished {
            reason: RuntimeFinishReason::Stop,
            ..
        }
    )));

    {
        let durable = world
            .context
            .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
            .unwrap();
        let durable = durable.lock().unwrap_or_else(|error| error.into_inner());
        assert!(durable.events().iter().any(|event| matches!(
            &event.kind,
            SessionEventKind::AssistantMessage { content, .. } if content == "hello"
        )));
    }

    let restart_error = match runtime
        .start(
            RuntimeStart::new(session_id(&world), world.workspace.path()).unwrap(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("a non-empty durable session must be resumed, not restarted"),
        Err(error) => error,
    };
    assert_eq!(restart_error.code(), RuntimeErrorCode::Conflict);

    let resumed = runtime
        .resume(
            RuntimeResume::new(
                session_id(&world),
                world.workspace.path(),
                session.id().clone(),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(resumed.id(), session.id());
    let fork_error = match runtime
        .fork(
            RuntimeFork::new(
                heycode_core::SessionId::from_raw("fork"),
                world.workspace.path(),
                session.id().clone(),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("native fork must stay unsupported until its owning task"),
        Err(error) => error,
    };
    assert_eq!(fork_error.code(), RuntimeErrorCode::Unsupported);

    world.context.shutdown();
    assert!(registry.get("native").unwrap().is_none());
    assert_eq!(
        session
            .send(
                RuntimeInput::new("after shutdown").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Cancelled
    );
}

#[tokio::test]
async fn native_runtime_cancel_settles_an_aborted_turn_and_close_refuses_new_work() {
    let mut world = world(Arc::new(HangThenReply {
        calls: std::sync::atomic::AtomicUsize::new(0),
    }));
    let session = start_native(&world).await;
    let mut events = session.subscribe();
    let running = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .send(RuntimeInput::new("hang").unwrap(), CancellationToken::new())
                .await
        })
    };
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if matches!(
            event.kind(),
            RuntimeEventKind::CommentaryDelta { text } if text == "partial"
        ) {
            break;
        }
    }
    session.cancel(CancellationToken::new()).await.unwrap();
    assert_eq!(
        running.await.unwrap().unwrap_err().code(),
        RuntimeErrorCode::Cancelled
    );
    let durable = world
        .context
        .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    assert!(
        durable
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .any(|event| matches!(
                event.kind,
                SessionEventKind::TurnEnd {
                    reason: TurnEndReason::Aborted,
                    ..
                }
            ))
    );

    let recovered = session
        .send(
            RuntimeInput::new("second turn").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(recovered.as_str(), "2");

    session.close(CancellationToken::new()).await.unwrap();
    session.close(CancellationToken::new()).await.unwrap();
    assert_eq!(
        session
            .send(
                RuntimeInput::new("after close").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Closed
    );
    world.context.shutdown();
}

/// Collect one turn's normalized event kinds, or the normalizer's error.
async fn drain_turn(
    events: &mut heycode_runtime::NormalizedRuntimeEventStream,
) -> Result<Vec<RuntimeEventKind>, heycode_runtime::RuntimeError> {
    let mut kinds = Vec::new();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
            .await
            .expect("the turn settles within the timeout")
            .expect("the stream stays open until the turn finishes")?;
        let terminal = matches!(event.kind(), RuntimeEventKind::TurnFinished { .. });
        kinds.push(event.kind().clone());
        if terminal {
            return Ok(kinds);
        }
    }
}

fn phase_name(kind: &RuntimeEventKind) -> &'static str {
    match kind {
        RuntimeEventKind::SessionReady => "session_ready",
        RuntimeEventKind::TurnStarted { .. } => "turn_started",
        RuntimeEventKind::CommentaryDelta { .. } => "commentary_delta",
        RuntimeEventKind::ReasoningDelta { .. } => "reasoning_delta",
        RuntimeEventKind::FinalMessage { .. } => "final_message",
        RuntimeEventKind::ToolCall { .. } => "tool_call",
        RuntimeEventKind::ToolResult { .. } => "tool_result",
        RuntimeEventKind::PermissionRequested { .. } => "permission_requested",
        RuntimeEventKind::QuestionRequested { .. } => "question_requested",
        RuntimeEventKind::ContextBudgetChanged { .. } => "context_budget_changed",
        RuntimeEventKind::Usage { .. } => "usage",
        RuntimeEventKind::TurnFinished { .. } => "turn_finished",
        RuntimeEventKind::Notice { .. } => "notice",
    }
}

fn text_then_tool_step(text: &str, path: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some("call-1".to_owned()),
            name: Some("read".to_owned()),
            arguments_delta: format!("{{\"path\":\"{path}\"}}"),
        },
        StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
    ]
}

fn text_step(text: &str) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    if !text.is_empty() {
        chunks.push(StreamChunk::TextDelta(text.to_owned()));
    }
    chunks.push(StreamChunk::Finish(heycode_llm::FinishReason::Stop));
    chunks
}

/// A step that speaks and then calls a tool is the normal shape of a turn for
/// every real provider. The runtime stream it produces must pass R02
/// normalization: the text is commentary, the tool call follows it, and the
/// single final message closes the turn.
#[tokio::test]
async fn text_then_tool_call_step_normalizes_cleanly() {
    let provider = Arc::new(FakeProvider::new(vec![
        text_then_tool_step("Let me check the README.", "README.md"),
        text_step("It is a Rust workspace."),
    ]));
    let world = world(provider);
    std::fs::write(world.workspace.path().join("README.md"), "# hi\n").unwrap();
    let session = start_native(&world).await;
    let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
    session
        .send(
            RuntimeInput::new("what is in the readme?").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let kinds = drain_turn(&mut events).await.expect("an R02-valid stream");
    let names: Vec<&str> = kinds
        .iter()
        .filter(|kind| !matches!(kind, RuntimeEventKind::ContextBudgetChanged { .. }))
        .map(phase_name)
        .collect();
    assert_eq!(
        names,
        [
            "session_ready",
            "turn_started",
            "commentary_delta",
            "tool_call",
            "tool_result",
            "commentary_delta",
            "final_message",
            "turn_finished",
        ],
        "{kinds:#?}"
    );
    assert!(matches!(
        kinds.last(),
        Some(RuntimeEventKind::TurnFinished {
            reason: RuntimeFinishReason::Stop,
            ..
        })
    ));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        RuntimeEventKind::FinalMessage { text } if text == "It is a Rust workspace."
    )));
}

/// An empty stop reply is legal model output and must not poison the session:
/// the final message is empty, not absent.
#[tokio::test]
async fn empty_stop_reply_emits_an_empty_final_message() {
    let provider = Arc::new(FakeProvider::new(vec![text_step("")]));
    let world = world(provider);
    let session = start_native(&world).await;
    let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
    session
        .send(RuntimeInput::new("hi").unwrap(), CancellationToken::new())
        .await
        .unwrap();
    let kinds = drain_turn(&mut events).await.expect("an R02-valid stream");
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        RuntimeEventKind::FinalMessage { text } if text.is_empty()
    )));
}

/// The second turn of a session must not inherit a poisoned stream from the
/// first: consecutive turns, including an empty one, all settle validly.
#[tokio::test]
async fn consecutive_turns_including_an_empty_one_all_settle() {
    let provider = Arc::new(FakeProvider::new(vec![
        text_step("one"),
        text_step(""),
        text_step("three"),
    ]));
    let world = world(provider);
    let session = start_native(&world).await;
    let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
    for prompt in ["a", "b", "c"] {
        session
            .send(RuntimeInput::new(prompt).unwrap(), CancellationToken::new())
            .await
            .unwrap();
        drain_turn(&mut events)
            .await
            .unwrap_or_else(|error| panic!("turn `{prompt}` produced an invalid stream: {error}"));
    }
}

/// A subscriber that attaches after more than the retained window of events
/// still receives a contiguous stream from sequence zero: retention must never
/// make a long session unsubscribable.
#[tokio::test]
async fn late_subscriber_after_eviction_gets_a_valid_stream() {
    let script = (0..300).map(|_| text_step("reply")).collect();
    let provider = Arc::new(FakeProvider::new(script));
    let world = world(provider);
    let session = start_native(&world).await;
    for _ in 0..300 {
        session
            .send(RuntimeInput::new("go").unwrap(), CancellationToken::new())
            .await
            .unwrap();
    }
    let mut late = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), late.next())
        .await
        .unwrap()
        .unwrap()
        .expect("a late subscription starts at a valid session-ready event");
    assert_eq!(first.sequence(), 0);
    assert!(matches!(first.kind(), RuntimeEventKind::SessionReady));
}

#[tokio::test]
async fn native_pending_turn_claims_exact_occurrence_once_and_preserves_cancellation() {
    let provider = Arc::new(FakeProvider::repeating(vec![
        StreamChunk::TextDelta("done".into()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]));
    let mut world = world(provider);
    let agent = world
        .context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let runtime = start_native(&world).await;
    let (first, _) = agent
        .submit_inbox(
            heycode_session::InboxDelivery::FollowUp,
            "first durable input",
        )
        .unwrap();
    let (second, _) = agent
        .submit_inbox(
            heycode_session::InboxDelivery::FollowUp,
            "second durable input",
        )
        .unwrap();
    assert!(
        runtime
            .send_pending(second.to_string(), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(agent.pending_inbox().next_turn, 2);
    let admitted_turn = runtime
        .send_pending(first.to_string(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(agent.pending_inbox().next_turn, 1);
    assert_eq!(
        runtime
            .send_pending(first.to_string(), CancellationToken::new())
            .await
            .unwrap(),
        admitted_turn
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        runtime
            .send_pending(second.to_string(), cancelled)
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Cancelled
    );
    assert_eq!(agent.pending_inbox().next_turn, 1);
    runtime
        .send_pending(second.to_string(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(agent.pending_inbox().next_turn, 0);
    {
        let session = agent.session().lock().unwrap();
        let messages: Vec<_> = session
            .events()
            .iter()
            .filter_map(|e| match &e.kind {
                SessionEventKind::UserMessage { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages,
            vec!["first durable input", "second durable input"]
        );
    }
    runtime.close(CancellationToken::new()).await.unwrap();
    world.context.shutdown();
}

#[tokio::test]
async fn native_pending_steer_uses_exact_step_occurrence_and_reuses_claimed_turn() {
    let provider = Arc::new(FakeProvider::repeating(vec![
        StreamChunk::TextDelta("done".into()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]));
    let mut world = world(provider);
    let agent = world
        .context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let runtime = start_native(&world).await;
    let (id, _) = agent
        .submit_inbox(heycode_session::InboxDelivery::Steer, "agent finding")
        .unwrap();
    let turn = runtime
        .send_pending(id.to_string(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        runtime
            .send_pending(id.to_string(), CancellationToken::new())
            .await
            .unwrap(),
        turn
    );
    assert!(agent.pending_inbox().is_empty());
    assert_eq!(
        agent
            .session()
            .lock()
            .unwrap()
            .events()
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnStart { .. }))
            .count(),
        1
    );
    runtime.close(CancellationToken::new()).await.unwrap();
    world.context.shutdown();
}

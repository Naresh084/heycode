//! `task` subagents: delegation, durable child sessions, depth enforcement.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, AutoApprove, agent_options_plugin, agent_plugin, approval_plugin,
    commands_plugin, subagent_jobs_plugin, subagent_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, LlmSelection, Provider, ProviderInfo, StreamChunk, llm_plugin,
    model_catalog_plugin,
};
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

// These scripted round trips require the child answer before the next parent
// response. Background admission is tested separately with the real default.
fn call(id: &str, mut args: serde_json::Value) -> StreamChunk {
    args.as_object_mut()
        .unwrap()
        .entry("background")
        .or_insert(serde_json::json!(false));
    args.as_object_mut()
        .unwrap()
        .entry("mode")
        .or_insert(serde_json::json!("oneshot"));
    call_named(id, "task", args)
}

fn call_named(id: &str, name: &str, args: serde_json::Value) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index: 0,
        id: Some(id.to_owned()),
        name: Some(name.to_owned()),
        arguments_delta: args.to_string(),
    }
}

fn world(
    scripts: Vec<Vec<StreamChunk>>,
    root: std::path::PathBuf,
    max_depth: u32,
) -> heycode_core::Context {
    world_ordered(scripts, root, max_depth, PlanOrder::AfterSubagent)
}

/// Which side of `subagent` the `plan` plugin is composed on. The delegation
/// gate must be installed either way; the shipped composition is
/// [`PlanOrder::AfterSubagent`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanOrder {
    AfterSubagent,
    BeforeSubagent,
}

fn world_ordered(
    scripts: Vec<Vec<StreamChunk>>,
    root: std::path::PathBuf,
    max_depth: u32,
    plan_order: PlanOrder,
) -> heycode_core::Context {
    world_ordered_budget(
        scripts,
        root,
        max_depth,
        plan_order,
        heycode_agent::SubagentBudgetLimits::default(),
    )
}

fn world_ordered_budget(
    scripts: Vec<Vec<StreamChunk>>,
    root: std::path::PathBuf,
    max_depth: u32,
    plan_order: PlanOrder,
    budget: heycode_agent::SubagentBudgetLimits,
) -> heycode_core::Context {
    let dir = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
    let _ = dir.keep(); // leak for world lifetime simplicity in tests
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(root.clone()),
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
            vec![Arc::new(FakeProvider::new(scripts))],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        heycode_agent::compactions_plugin(),
    ];
    match plan_order {
        PlanOrder::AfterSubagent => {
            plugins.push(heycode_agent::subagent_plugin_with_budget(
                root.clone(),
                max_depth,
                budget,
            ));
            plugins.push(heycode_agent::plan_plugin());
        }
        PlanOrder::BeforeSubagent => {
            plugins.push(heycode_agent::plan_plugin());
            plugins.push(heycode_agent::subagent_plugin_with_budget(
                root.clone(),
                max_depth,
                budget,
            ));
        }
    }
    plugins.push(agent_options_plugin(AgentOptions::default()));
    plugins.push(agent_plugin());
    plugins.push(subagent_jobs_plugin());
    compose(&plugins).unwrap()
}

/// Turn plan mode on through the same `/plan` command a user would run.
async fn plan_on(context: &heycode_core::Context) {
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&agent, "on")
        .await
        .unwrap();
}

/// A provider that drives an agent OUTSIDE this process: it never sees the
/// parent's `seam/pre_tool` waterfall, so its child can mutate the workspace
/// no matter what the parent's guards say. `write` here stands in for that.
struct DelegatedFixtureProvider {
    descriptor: heycode_agent::SubagentProviderDescriptor,
    started: Arc<std::sync::atomic::AtomicUsize>,
    victim: std::path::PathBuf,
}

#[async_trait::async_trait]
impl heycode_agent::SubagentProvider for DelegatedFixtureProvider {
    fn descriptor(&self) -> &heycode_agent::SubagentProviderDescriptor {
        &self.descriptor
    }

    async fn start(
        &self,
        _request: heycode_agent::SubagentRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::SubagentStarted, heycode_agent::SubagentError> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(&self.victim, "written by a delegated child\n").unwrap();
        Ok(heycode_agent::SubagentStarted {
            id: heycode_agent::SubagentId::new("delegated-child").unwrap(),
            text: "delegated child wrote the file".to_owned(),
            handle: None,
        })
    }
}

fn attach_delegated_fixture(
    context: &heycode_core::Context,
    victim: std::path::PathBuf,
) -> Arc<std::sync::atomic::AtomicUsize> {
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry
        .register(Arc::new(DelegatedFixtureProvider {
            descriptor: heycode_agent::SubagentProviderDescriptor::new(
                "delegated-fixture",
                "Delegated fixture",
                heycode_agent::SubagentCapabilities {
                    fork: heycode_llm::CapabilitySupport::Unsupported,
                    continuation: heycode_llm::CapabilitySupport::Unsupported,
                    interrupt: heycode_llm::CapabilitySupport::Unsupported,
                },
            )
            .unwrap(),
            started: started.clone(),
            victim,
        }))
        .unwrap();
    started
}

struct BackgroundFixtureProvider {
    started: Arc<tokio::sync::Notify>,
    descriptor: heycode_agent::SubagentProviderDescriptor,
    release: Arc<tokio::sync::Notify>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl heycode_agent::SubagentProvider for BackgroundFixtureProvider {
    fn descriptor(&self) -> &heycode_agent::SubagentProviderDescriptor {
        &self.descriptor
    }

    fn supports_configuration(&self) -> bool {
        true
    }

    async fn start(
        &self,
        request: heycode_agent::SubagentRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::SubagentStarted, heycode_agent::SubagentError> {
        self.started.notify_one();
        tokio::select! {
            () = cancellation.cancelled() => {
                self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(heycode_agent::SubagentError::new(
                    heycode_agent::SubagentErrorCode::Cancelled,
                    "cancelled",
                ))
            }
            () = self.release.notified() => Ok(heycode_agent::SubagentStarted {
                id: heycode_agent::SubagentId::new("background-child").unwrap(),
                text: format!("background result for {}", request.prompt()),
                handle: None,
            }),
        }
    }
}

fn attach_background_fixture(
    context: &heycode_core::Context,
) -> (
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
    Arc<std::sync::atomic::AtomicBool>,
    heycode_agent::SubagentPresetRegistration,
) {
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    registry
        .register(Arc::new(BackgroundFixtureProvider {
            started: started.clone(),
            descriptor: heycode_agent::SubagentProviderDescriptor::new(
                "background-fixture",
                "Background fixture",
                heycode_agent::SubagentCapabilities {
                    fork: heycode_llm::CapabilitySupport::Unsupported,
                    continuation: heycode_llm::CapabilitySupport::Unsupported,
                    interrupt: heycode_llm::CapabilitySupport::Supported,
                },
            )
            .unwrap(),
            release: release.clone(),
            cancelled: cancelled.clone(),
        }))
        .unwrap();
    let registration = registry
        .register_preset_owned(
            heycode_agent::SubagentPreset::new(
                "background-test",
                "Background test",
                "Run the background fixture.",
                Some(heycode_agent::SubagentProviderId::new("background-fixture").unwrap()),
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
            )
            .unwrap(),
        )
        .unwrap();
    (release, started, cancelled, registration)
}

fn background_job_id(value: &serde_json::Value) -> String {
    assert_eq!(value["delivery"], "automatic");
    assert!(value["agent_id"].as_str().is_some());
    value["job_id"]
        .as_str()
        .expect("structured spawn receipt job id")
        .to_owned()
}

async fn wait_for_job(jobs: &heycode_agent::JobRegistry, id: &str) -> heycode_agent::JobState {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(state) = jobs
                .list()
                .into_iter()
                .find(|job| job.id.as_str() == id)
                .map(|job| job.state)
                && matches!(state, heycode_agent::JobState::Settled(_))
            {
                return state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn default_background_task_returns_immediately_then_commits_result_to_durable_inbox() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(Vec::new(), root.path().to_path_buf(), 3);
    let (release, _started, _cancelled, _preset) = attach_background_fixture(&context);
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let output = tools
        .get("task")
        .unwrap()
        .run(
            serde_json::json!({
                "label":"background review",
                "prompt":"check this",
                "agent":"background-test"
            }),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    let id = background_job_id(&output);
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert!(matches!(
        jobs.list()[0].state,
        heycode_agent::JobState::Running | heycode_agent::JobState::Queued
    ));
    release.notify_one();
    assert_eq!(
        wait_for_job(&jobs, &id).await,
        heycode_agent::JobState::Settled(heycode_agent::JobOutcome::Completed)
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent
        .wait_for_background(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert!(agent.pending_inbox().is_empty());
    let session = agent
        .session()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(session.events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::AgentInboxSplice { inserted, .. }
            if inserted.iter().any(|message| {
                message.text().contains(&id) && message.text().contains("background result")
            })
    )));
    drop(session);
    context.shutdown();
}

#[tokio::test]
async fn cancelling_background_subagent_propagates_and_commits_cancelled_settlement() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(Vec::new(), root.path().to_path_buf(), 3);
    let (_release, started, cancelled, _preset) = attach_background_fixture(&context);
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let output = tools
        .get("task")
        .unwrap()
        .run(
            serde_json::json!({
                "label":"cancel review",
                "prompt":"wait",
                "agent":"background-test",
                "background":true
            }),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    let id = background_job_id(&output);
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let cancelled_output = tools
        .get("cancel_job")
        .unwrap()
        .run(
            serde_json::json!({"job_id":id}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        cancelled_output.as_str(),
        Some(format!("cancelled {id}").as_str())
    );
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        wait_for_job(&jobs, &id).await,
        heycode_agent::JobState::Settled(heycode_agent::JobOutcome::Cancelled)
    );
    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    context.shutdown();
}

#[tokio::test]
async fn task_delegates_to_a_fresh_durable_child_and_returns_its_answer() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![
            // parent step 1: delegate
            vec![
                call(
                    "t1",
                    serde_json::json!({"label": "research", "prompt": "find the answer"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            // child turn (fresh agent, fresh session)
            stop_text("CHILD-RESULT-42"),
            // parent step 2: incorporate
            stop_text("parent done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let report = agent.send("delegate research").await.unwrap();
    assert_eq!(report.text, "parent done");

    // The child's answer reached the parent as the tool result.
    let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
    assert!(s.events().iter().any(|e| matches!(
        &e.kind,
        heycode_session::SessionEventKind::ToolResult { content, is_error: false, .. }
            if content.contains("CHILD-RESULT-42")
    )));

    // The child session is durable under the runner root (>=1 child dir).
    let child_dirs = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .count();
    assert!(child_dirs >= 1, "child session must persist");
}

#[tokio::test]
async fn depth_limit_refuses_nested_spawns_loudly() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![vec![
            call("t1", serde_json::json!({"label": "x", "prompt": "y"})),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ]],
        root.path().to_path_buf(),
        0, // max_depth 0: every spawn refuses
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("try to delegate").await.unwrap();

    let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
    assert!(s.events().iter().any(|e| matches!(
        &e.kind,
        heycode_session::SessionEventKind::ToolResult { content, is_error: true, .. }
            if content.contains("depth limit 0")
    )));
}

#[tokio::test]
async fn continuable_children_survive_for_follow_ups() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![
            // parent: delegate continuable
            vec![
                call(
                    "t1",
                    serde_json::json!({
                        "label": "research",
                        "prompt": "find it",
                        "mode": "continuable"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            // child turn 1
            stop_text("CHILD-FIRST"),
            // parent step 2
            stop_text("got it"),
            // child follow-up turn (send_message)
            stop_text("CHILD-SECOND"),
            // parent final
            stop_text("done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("start research").await.unwrap();

    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tasks = tools
        .get("list_tasks")
        .unwrap()
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    let task_id = tasks["tasks"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(tasks["tasks"][0]["state"], "idle");

    let reply = tools
        .get("send_message")
        .unwrap()
        .run(
            serde_json::json!({"task_id": task_id, "message": "go deeper"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(reply.as_str().unwrap(), "CHILD-SECOND");

    // The SAME durable child session grew across both turns.
    let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
    assert!(s.events().iter().any(|e| matches!(
        &e.kind,
        heycode_session::SessionEventKind::ToolResult { content, is_error: false, .. }
            if content.contains("CHILD-FIRST")
    )));
}

#[tokio::test]
async fn foreground_oneshot_leaves_no_live_task() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![
            vec![
                call("t1", serde_json::json!({"label": "x", "prompt": "y"})),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child says"),
            stop_text("fine"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("one-shot").await.unwrap();

    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tasks = tools
        .get("list_tasks")
        .unwrap()
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(tasks["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(tasks["tasks"][0]["state"], "completed");
}

#[tokio::test]
async fn fork_mode_seeds_child_with_parent_history() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![
            // turn 1: parent learns a fact (plain reply)
            stop_text("SECRET-CODE-7391 remembered"),
            // turn 2: parent delegates in FORK mode
            vec![
                call(
                    "f1",
                    serde_json::json!({
                        "label": "heir",
                        "prompt": "What is the secret code? Reply with just the number.",
                        "mode": "fork"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            // child (forked) sees the history and answers from it
            stop_text("the code is 7391"),
            // parent wraps up
            stop_text("fork done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    agent.send("remember SECRET-CODE-7391").await.unwrap();
    agent.send("delegate with fork").await.unwrap();

    let parent_id = agent
        .session()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .id()
        .clone();
    // A fork stores only its suffix while replay verifies and stitches the
    // exact parent prefix. The child must not duplicate prompt/history bytes.
    let child_dirs: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("session.jsonl").is_file())
        .collect();
    let child = child_dirs
        .into_iter()
        .filter_map(|path| heycode_session::Session::open(path).ok())
        .find(|session| session.lineage().is_some())
        .expect("forked child session");
    assert_eq!(child.lineage().unwrap().parent_session_id(), &parent_id);
    assert!(child.first_local_seq() > 0);
    assert!(child.events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::UserMessage { text }
            if text == "remember SECRET-CODE-7391"
    )));
    assert_eq!(
        child
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                heycode_session::SessionEventKind::UserMessage { text }
                    if text == "What is the secret code? Reply with just the number."
            ))
            .count(),
        1
    );
    let local = std::fs::read_to_string(child.path()).unwrap();
    assert!(local.contains("\"kind\":\"session/created\""));
    assert!(!local.contains("SECRET-CODE-7391"));
    assert!(!local.contains("[Forked from parent"));

    let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
    assert!(s.events().iter().any(|e| matches!(
        &e.kind,
        heycode_session::SessionEventKind::ToolResult { content, is_error: false, .. }
            if content.contains("7391")
    )));
}

#[tokio::test]
async fn fork_mode_from_the_first_open_turn_retains_pre_turn_lineage() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![
            vec![
                call(
                    "f0",
                    serde_json::json!({
                        "label": "first-heir",
                        "prompt": "work from an empty inherited prefix",
                        "mode": "fork"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("first child done"),
            stop_text("parent done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("delegate immediately").await.unwrap();

    let child = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| heycode_session::Session::open(entry.path()).ok())
        .find(|session| session.lineage().is_some())
        .expect("first-turn forked child");
    assert_eq!(child.lineage().unwrap().seed_event_count(), 1);
    assert!(child.events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::UserMessage { text }
            if text == "delegate immediately"
    )));
    assert!(child.events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::UserMessage { text }
            if text == "work from an empty inherited prefix"
    )));
}

#[tokio::test]
async fn oneshot_child_stays_blind_to_parent_history() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    struct Rec {
        inner: FakeProvider,
        sink: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    }
    impl Provider for Rec {
        fn info(&self) -> ProviderInfo {
            self.inner.info()
        }
        fn stream(&self, request: ChatRequest) -> ChunkStream {
            self.sink.lock().unwrap().push(request.clone());
            self.inner.stream(request)
        }
    }
    let provider: Arc<dyn Provider> = Arc::new(Rec {
        inner: FakeProvider::new(vec![
            stop_text("noted"),
            vec![
                call(
                    "t1",
                    serde_json::json!({"label": "blind", "prompt": "what code?"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("I have no idea"),
            stop_text("right"),
        ]),
        sink: requests.clone(),
    });

    let dir = tempfile::tempdir().unwrap();
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
        heycode_agent::compactions_plugin(),
        subagent_plugin(dir.path().to_path_buf(), 3),
        agent_options_plugin(AgentOptions::default()),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("remember SECRET-CODE-7391").await.unwrap();
    agent.send("delegate blind").await.unwrap();

    // Request order: [0] parent t1, [1] parent t2 (delegation call),
    // [2] CHILD, [3] parent wrap-up. Only [2] must be blind to the secret.
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 4);
    let child_req = &reqs[2];
    assert!(
        !child_req
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-CODE-7391")),
        "oneshot children stay blind"
    );
}

// ---------------------------------------------------------------------------
// O01 — provider/continuation contract
// ---------------------------------------------------------------------------

/// Provider that proves nothing beyond a fresh one-shot run, used to show that
/// selection is driven by evidence rather than by registration luck.
struct MinimalProvider {
    descriptor: heycode_agent::SubagentProviderDescriptor,
    started: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl heycode_agent::SubagentProvider for MinimalProvider {
    fn descriptor(&self) -> &heycode_agent::SubagentProviderDescriptor {
        &self.descriptor
    }

    async fn start(
        &self,
        request: heycode_agent::SubagentRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::SubagentStarted, heycode_agent::SubagentError> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(heycode_agent::SubagentStarted {
            id: heycode_agent::SubagentId::new("minimal-1").unwrap(),
            text: format!("minimal:{}", request.label()),
            handle: None,
        })
    }
}

fn minimal(
    id: &str,
    started: Arc<std::sync::atomic::AtomicUsize>,
) -> Arc<dyn heycode_agent::SubagentProvider> {
    let unknown = heycode_llm::CapabilitySupport::Unknown;
    Arc::new(MinimalProvider {
        descriptor: heycode_agent::SubagentProviderDescriptor::new(
            id,
            "Minimal provider",
            heycode_agent::SubagentCapabilities {
                fork: unknown,
                continuation: unknown,
                interrupt: unknown,
            },
        )
        .unwrap(),
        started,
    })
}

#[tokio::test]
async fn the_registry_selects_by_evidence_and_refuses_unproven_combinations() {
    let registry = heycode_agent::SubagentRegistry::new();
    let authority = registry.root_authority(heycode_agent::SubagentId::new("root").unwrap());
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry
        .register(minimal("minimal", started.clone()))
        .unwrap();

    let fresh = heycode_agent::SubagentRequest::with_authority(
        "look",
        "find it",
        heycode_agent::SubagentSeed::Fresh,
        heycode_agent::SubagentContinuation::OneShot,
        authority.clone(),
    )
    .unwrap();
    assert_eq!(
        registry
            .start(fresh, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap()
            .text,
        "minimal:look"
    );

    for unproven in [
        (
            heycode_agent::SubagentSeed::ForkParent,
            heycode_agent::SubagentContinuation::OneShot,
        ),
        (
            heycode_agent::SubagentSeed::Fresh,
            heycode_agent::SubagentContinuation::Continuable,
        ),
    ] {
        let request = heycode_agent::SubagentRequest::with_authority(
            "look",
            "find it",
            unproven.0,
            unproven.1,
            authority.clone(),
        )
        .unwrap();
        let error = registry
            .start(request, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), heycode_agent::SubagentErrorCode::Unsupported);
    }
    assert_eq!(started.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn duplicate_provider_registration_fails_loud() {
    let registry = heycode_agent::SubagentRegistry::new();
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry
        .register(minimal("native", started.clone()))
        .unwrap();
    let error = registry
        .register(minimal("native", started))
        .expect_err("a second claimant for a live id must fail");
    assert_eq!(error.code(), heycode_agent::SubagentErrorCode::Refused);
    assert_eq!(registry.descriptors().len(), 1);
}

#[tokio::test]
async fn the_composed_world_publishes_a_native_provider_that_proves_every_mode() {
    let root = tempfile::tempdir().unwrap().keep();
    let ctx = world(vec![stop_text("unused")], root, 2);
    let registry = ctx
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .expect("subagent registry must be a composed service");
    let descriptors = registry.descriptors();
    assert_eq!(descriptors.len(), 1);
    let native = &descriptors[0];
    assert_eq!(native.id().as_str(), "native");
    let supported = heycode_llm::CapabilitySupport::Supported;
    assert_eq!(native.capabilities().fork, supported);
    assert_eq!(native.capabilities().continuation, supported);
    assert_eq!(native.capabilities().interrupt, supported);
}

/// Plan mode admits `task` because the NATIVE child inherits the parent's
/// `seam/pre_tool` waterfall. A delegated child is an external agent process
/// that inherits nothing, so routing `task` at one while plan mode is on is
/// the same workspace-mutating escape as `background_shell`.
#[tokio::test]
async fn plan_mode_refuses_a_delegated_task_before_the_child_starts() {
    let root = tempfile::tempdir().unwrap();
    let victim = root.path().join("victim.txt");
    std::fs::write(&victim, "clean\n").unwrap();
    let ctx = world(
        vec![
            vec![
                call(
                    "t1",
                    serde_json::json!({
                        "label": "write it",
                        "prompt": "write the file",
                        "provider": "delegated-fixture"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("blocked"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let started = attach_delegated_fixture(&ctx, victim.clone());
    plan_on(&ctx).await;
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    agent.send("delegate the write").await.unwrap();

    assert_eq!(
        started.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "plan mode delegated to an external agent"
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "clean\n",
        "a delegated child mutated the workspace while plan mode was on"
    );
    let session = agent.session();
    let session = session.lock().unwrap_or_else(|error| error.into_inner());
    assert!(
        session.events().iter().any(|event| matches!(
            &event.kind,
            heycode_session::SessionEventKind::ToolResult { content, is_error: true, .. }
                if content.contains("plan mode is active")
        )),
        "the refusal must reach the model as a loud tool error"
    );
}

/// The gate is wired from whichever of `plan`/`subagent` composes second, so
/// neither order leaves delegation unguarded.
#[tokio::test]
async fn plan_mode_gates_delegation_in_either_composition_order() {
    for order in [PlanOrder::AfterSubagent, PlanOrder::BeforeSubagent] {
        let root = tempfile::tempdir().unwrap();
        let victim = root.path().join("victim.txt");
        std::fs::write(&victim, "clean\n").unwrap();
        let ctx = world_ordered(Vec::new(), root.path().to_path_buf(), 3, order);
        let started = attach_delegated_fixture(&ctx, victim.clone());
        plan_on(&ctx).await;
        let registry = ctx
            .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
            .unwrap();
        let authority = registry.root_authority(heycode_agent::SubagentId::new("root").unwrap());
        let request = heycode_agent::SubagentRequest::with_authority(
            "write it",
            "write the file",
            heycode_agent::SubagentSeed::Fresh,
            heycode_agent::SubagentContinuation::OneShot,
            authority,
        )
        .unwrap()
        .with_provider(heycode_agent::SubagentProviderId::new("delegated-fixture").unwrap());

        let error = registry
            .start(request, tokio_util::sync::CancellationToken::new())
            .await
            .expect_err("plan mode must refuse an unguarded delegation");

        assert_eq!(error.code(), heycode_agent::SubagentErrorCode::Refused);
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "clean\n");
    }
}

/// One registry, one gate: a second claimant would silently decide which
/// session-wide policy is enforced.
#[test]
fn a_second_delegation_gate_is_refused() {
    struct AlwaysRefuse;
    impl heycode_agent::DelegationGate for AlwaysRefuse {
        fn refuse_unguarded_delegation(
            &self,
            _provider: &heycode_agent::SubagentProviderId,
        ) -> Option<String> {
            Some("no".to_owned())
        }
    }

    let registry = heycode_agent::SubagentRegistry::new();
    registry
        .attach_delegation_gate(Arc::new(AlwaysRefuse))
        .unwrap();
    let error = registry
        .attach_delegation_gate(Arc::new(AlwaysRefuse))
        .expect_err("a second gate must fail loud");
    assert_eq!(error.code(), heycode_agent::SubagentErrorCode::Refused);
}

/// The other half of the contract: research delegation stays available in
/// plan mode, because the native child runs under the parent's own guards.
#[tokio::test]
async fn plan_mode_still_admits_a_native_task() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![
            vec![
                call(
                    "t1",
                    serde_json::json!({"label": "research", "prompt": "find the answer"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("CHILD-RESULT-42"),
            stop_text("parent done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    plan_on(&ctx).await;
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    let report = agent.send("research it").await.unwrap();

    assert_eq!(report.text, "parent done");
    let session = agent.session();
    let session = session.lock().unwrap_or_else(|error| error.into_inner());
    assert!(
        session.events().iter().any(|event| matches!(
            &event.kind,
            heycode_session::SessionEventKind::ToolResult { content, is_error: false, .. }
                if content.contains("CHILD-RESULT-42")
        )),
        "plan mode must keep native research delegation available"
    );
}

#[tokio::test]
async fn context_shutdown_releases_providers_and_live_children() {
    let root = tempfile::tempdir().unwrap().keep();
    let mut ctx = world(
        vec![
            vec![
                call(
                    "c1",
                    serde_json::json!({
                        "label": "kept",
                        "prompt": "stay alive",
                        "mode": "continuable"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child answered"),
            stop_text("parent done"),
        ],
        root,
        2,
    );
    let registry = ctx
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("delegate and keep").await.unwrap();
    assert_eq!(
        registry.live_child_count(),
        1,
        "continuable child is retained"
    );

    ctx.shutdown();
    assert!(
        registry.live_child_count() == 0,
        "shutdown must not leave an orphaned child holding a durable session"
    );
    assert!(registry.descriptors().is_empty());
}

#[tokio::test]
async fn a_nested_child_cannot_list_its_owners_sibling() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(
        vec![
            // Parent creates continuable child A.
            vec![
                call(
                    "a1",
                    serde_json::json!({
                        "label":"first child",
                        "prompt":"wait",
                        "mode":"continuable"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("A ready"),
            stop_text("parent after A"),
            // Parent creates child B.
            vec![
                call(
                    "b1",
                    serde_json::json!({
                        "label":"second child",
                        "prompt":"list only tasks you own",
                        "mode":"continuable"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            // B asks the shared model-facing registry for its tasks. A is a
            // sibling owned by the parent and must be invisible.
            vec![
                call_named("list-b", "list_tasks", serde_json::json!({})),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("B done"),
            stop_text("parent after B"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("start A").await.unwrap();
    agent.send("start B").await.unwrap();

    let child_with_listing = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| heycode_session::Session::open(entry.path()).ok())
        .find(|session| {
            session.events().iter().any(|event| {
                matches!(
                    &event.kind,
                    heycode_session::SessionEventKind::ToolResult { call_id, .. }
                        if call_id.as_str() == "list-b"
                )
            })
        })
        .expect("child B session");
    assert!(child_with_listing.events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::ToolResult {
            call_id,
            content,
            is_error: false,
            ..
        } if call_id.as_str() == "list-b" && serde_json::from_str::<serde_json::Value>(content).unwrap()["tasks"].as_array().unwrap().is_empty()
    )));
    ctx.shutdown();
}

#[tokio::test]
async fn a_one_shot_child_cannot_leave_a_continuable_descendant() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(
        vec![
            vec![
                call(
                    "parent-child",
                    serde_json::json!({"label":"one shot","prompt":"delegate once"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            // The one-shot child attempts to create work that would outlive
            // its own authority lifetime.
            vec![
                call(
                    "orphan-attempt",
                    serde_json::json!({
                        "label":"would orphan",
                        "prompt":"stay alive",
                        "mode":"continuable"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child handled refusal"),
            stop_text("parent done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("delegate one shot").await.unwrap();

    let refused = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| heycode_session::Session::open(entry.path()).ok())
        .any(|session| {
            session.events().iter().any(|event| {
                matches!(
                    &event.kind,
                    heycode_session::SessionEventKind::ToolResult {
                        call_id,
                        content,
                        is_error: true,
                        ..
                    } if call_id.as_str() == "orphan-attempt"
                        && content.contains("one-shot")
                        && content.contains("continuable")
                )
            })
        });
    assert!(refused, "the one-shot child must receive a durable refusal");
    let registry = ctx
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    assert_eq!(registry.live_child_count(), 0, "no orphan may stay live");
    ctx.shutdown();
}

#[tokio::test]
async fn a_continuable_follow_up_retains_depth_and_cannot_reset_the_limit() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(
        vec![
            vec![
                call(
                    "continuable",
                    serde_json::json!({
                        "label":"depth child",
                        "prompt":"start",
                        "mode":"continuable"
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child ready"),
            stop_text("parent ready"),
            // This is the child follow-up turn. At depth 1 with max_depth 1,
            // another delegation must be refused even though the follow-up is
            // entered later through send_message.
            vec![
                call(
                    "depth-reset-attempt",
                    serde_json::json!({"label":"nested","prompt":"escape limit"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child kept its depth"),
        ],
        root.path().to_path_buf(),
        1,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("create continuable").await.unwrap();
    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let listing = tools
        .get("list_tasks")
        .unwrap()
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    let id = listing["tasks"][0]["id"].as_str().unwrap();
    let reply = tools
        .get("send_message")
        .unwrap()
        .run(
            serde_json::json!({"task_id":id,"message":"try nested"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(reply.as_str(), Some("child kept its depth"));

    let child = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| heycode_session::Session::open(entry.path()).ok())
        .find(|session| {
            session.id().as_str() == listing["tasks"][0]["session_id"].as_str().unwrap()
        })
        .expect("continuable child session");
    assert!(child.events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::ToolResult {
            call_id,
            content,
            is_error: true,
            ..
        } if call_id.as_str() == "depth-reset-attempt" && content.contains("depth limit 1")
    )));
    ctx.shutdown();
}

/// The shipping task schema discovers native built-ins. Its description must
/// continue to distinguish fresh delegation from the inherited fork context.
#[test]
fn task_schema_lists_builtin_agents_and_describes_fork_honestly() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(Vec::new(), root.path().to_path_buf(), 3);
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let spec = tools.get("task").unwrap().spec();
    let properties = spec.parameters["properties"].as_object().unwrap();
    assert_eq!(
        properties["agent"]["enum"],
        serde_json::json!(["advisor", "reviewer", "security-review"])
    );
    assert!(
        spec.description.contains("`fork` inherits"),
        "{}",
        spec.description
    );
    context.shutdown();
}

struct HeldNativeProvider {
    requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    release: tokio_util::sync::CancellationToken,
}
impl Provider for HeldNativeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "held-native".into(),
            default_model: "initial".into(),
        }
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.requests.lock().unwrap().push(request);
        let release = self.release.clone();
        Box::pin(futures::stream::unfold(Some(release), |state| async move {
            let release = state?;
            release.cancelled().await;
            Some((
                Ok(StreamChunk::Finish(heycode_llm::FinishReason::Stop)),
                None,
            ))
        }))
    }
}
fn native_owner(
    ctx: &heycode_core::Context,
) -> (
    Arc<heycode_agent::SubagentRegistry>,
    heycode_agent::SubagentAuthority,
) {
    let registry = ctx
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let owner =
        heycode_agent::SubagentId::new(agent.session().lock().unwrap().id().as_str()).unwrap();
    let authority = registry.root_authority(owner);
    (registry, authority)
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn native_early_ids_three_live_children_cancel_and_restore_without_lost_turns() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(vec![], root.path().to_path_buf(), 4);
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = tokio_util::sync::CancellationToken::new();
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(HeldNativeProvider {
            requests: requests.clone(),
            release: release.clone(),
        }))
        .unwrap();
    agent.set_inference_route("held-native", "changed-after-composition", None);
    let (registry, authority) = native_owner(&ctx);
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_child = observed.clone();
    let _observer = registry.attach_native_child_observer(Arc::new(move |_, child| {
        assert!(
            !child.token().is_turn_active(),
            "observer must run before first send"
        );
        observed_child.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let mut admitted = Vec::new();
    for i in 0..3 {
        let request = heycode_agent::SubagentRequest::with_authority(
            format!("child {i}"),
            "held request",
            heycode_agent::SubagentSeed::Fresh,
            heycode_agent::SubagentContinuation::Continuable,
            authority.clone(),
        )
        .unwrap();
        let (id, job) = registry
            .start_background_task(request, heycode_session::InboxDelivery::Inject)
            .unwrap();
        assert!(
            registry
                .task_snapshots_for(&authority)
                .iter()
                .any(|row| row.id == id.as_str()),
            "identity must exist synchronously"
        );
        admitted.push((id, job));
    }
    until(|| requests.lock().unwrap().len() == 3).await;
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(requests.lock().unwrap().iter().all(|request| request.model
        == "changed-after-composition"
        && request.max_tokens.is_none()));
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    for (id, job) in &admitted {
        assert!(registry.native_child_for(&authority, id).is_some());
        assert!(
            registry
                .task_snapshots_for(&authority)
                .iter()
                .find(|row| row.id == id.as_str())
                .unwrap()
                .session_id
                .is_some()
        );
        assert!(registry.interrupt_for(&authority, id));
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                jobs.wait_for_settlement(job)
            )
            .await
            .unwrap()
            .unwrap(),
            heycode_agent::JobOutcome::Cancelled
        );
        assert!(
            registry.child_for(&authority, id).is_some(),
            "interrupt preserves continuable child"
        );
    }
    let (id, _) = &admitted[0];
    assert!(registry.archive_child_for(&authority, id).unwrap());
    assert!(registry.child_for(&authority, id).is_none());
    assert!(registry.restore_child_for(&authority, id).unwrap());
    release.cancel();
    let job = registry
        .send_background(
            &authority,
            id,
            "continue".into(),
            false,
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    assert_eq!(
        jobs.wait_for_settlement(&job).await.unwrap(),
        heycode_agent::JobOutcome::Completed
    );
    let foreign = registry.root_authority(heycode_agent::SubagentId::new("foreign-owner").unwrap());
    assert!(registry.native_child_for(&foreign, id).is_none());
    assert!(!registry.restore_child_for(&foreign, id).unwrap());
    assert!(
        registry
            .send_background(
                &foreign,
                id,
                "forbidden".into(),
                false,
                heycode_session::InboxDelivery::Inject
            )
            .is_err()
    );
}

#[tokio::test]
async fn stop_before_first_poll_prevents_native_inference_entirely() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(vec![], root.path().to_path_buf(), 4);
    let (registry, authority) = native_owner(&ctx);
    let (id, job) = registry
        .start_background_task(
            heycode_agent::SubagentRequest::with_authority(
                "early stop",
                "do not run",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
                authority.clone(),
            )
            .unwrap(),
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    assert!(registry.interrupt_for(&authority, &id));
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        jobs.wait_for_settlement(&job).await.unwrap(),
        heycode_agent::JobOutcome::Cancelled
    );
    assert_eq!(registry.budget_snapshot().requests_reserved, 0);
    assert_eq!(
        registry.task_snapshots_for(&authority)[0].state,
        heycode_agent::TaskState::Cancelled
    );
}

struct CancellableChildTool {
    started: Arc<std::sync::atomic::AtomicBool>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait::async_trait]
impl heycode_tools::Tool for CancellableChildTool {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "held_child_tool".into(),
            description: "fixture".into(),
            parameters: serde_json::json!({"type":"object"}),
        }
    }
    async fn run(
        &self,
        _: serde_json::Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        self.started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        cx.cancellation.cancelled().await;
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Err(heycode_tools::ToolError::new(
            "cancelled before side effect",
        ))
    }
}
#[tokio::test]
async fn first_child_tool_is_cancelled_and_joined_before_job_settlement() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(
        vec![vec![
            call_named("hold", "held_child_tool", serde_json::json!({})),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ]],
        root.path().to_path_buf(),
        4,
    );
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    ctx.get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .register_shared(Arc::new(CancellableChildTool {
            started: started.clone(),
            cancelled: cancelled.clone(),
        }))
        .unwrap();
    let (registry, authority) = native_owner(&ctx);
    let (id, job) = registry
        .start_background_task(
            heycode_agent::SubagentRequest::with_authority(
                "tool cancellation",
                "run tool",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
                authority.clone(),
            )
            .unwrap(),
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    until(|| started.load(std::sync::atomic::Ordering::SeqCst)).await;
    assert!(registry.interrupt_for(&authority, &id));
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            jobs.wait_for_settlement(&job)
        )
        .await
        .unwrap()
        .unwrap(),
        heycode_agent::JobOutcome::Cancelled
    );
    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
}

fn native_worktree_world(
    repository: &std::path::Path,
    sessions: &std::path::Path,
    sandbox: heycode_exec::SandboxService,
    scripts: Vec<Vec<StreamChunk>>,
) -> heycode_core::Context {
    let config =
        heycode_exec::LocalShellConfig::platform(repository, std::time::Duration::from_secs(30))
            .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(sessions.to_path_buf()),
        prompt_plugin(),
        heycode_exec::local_execution_plugin_with_sandbox(config, sandbox),
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
            vec![Arc::new(FakeProvider::new(scripts))],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        heycode_agent::compactions_plugin(),
        subagent_plugin(sessions.to_path_buf(), 4),
        agent_options_plugin(AgentOptions {
            cwd: Some(repository.to_path_buf()),
            ..AgentOptions::default()
        }),
        agent_plugin(),
        subagent_jobs_plugin(),
    ];
    compose(&plugins).unwrap()
}
fn repo_git(path: &std::path::Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}
async fn native_worktree_lifecycle(sandboxed: bool) {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    repo_git(&repository, &["init", "-q"]);
    std::fs::write(repository.join("tracked.txt"), "base\n").unwrap();
    repo_git(&repository, &["add", "."]);
    repo_git(
        &repository,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "base",
        ],
    );
    std::fs::write(repository.join("tracked.txt"), "dirty\n").unwrap();
    std::fs::write(repository.join("seed.txt"), "untracked input\n").unwrap();
    let sandbox = heycode_exec::SandboxService::new(
        if sandboxed {
            heycode_exec::SandboxMode::WorkspaceWrite
        } else {
            heycode_exec::SandboxMode::Off
        },
        &repository,
        if sandboxed {
            Some(heycode_sandbox::platform_default().unwrap())
        } else {
            None
        },
    )
    .unwrap();
    let scripts = vec![
        vec![
            call_named("read", "read", serde_json::json!({"path":"tracked.txt"})),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        vec![
            call_named(
                "edit",
                "edit",
                serde_json::json!({"path":"tracked.txt","old_string":"dirty","new_string":"child"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        vec![
            call_named(
                "shell",
                "bash",
                serde_json::json!({"command":"printf child > shell.txt"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("initial complete"),
        vec![
            call_named(
                "follow",
                "bash",
                serde_json::json!({"command":"printf followup >> shell.txt"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("followup complete"),
    ];
    let ctx = native_worktree_world(&repository, &root.path().join("sessions"), sandbox, scripts);
    let (registry, authority) = native_owner(&ctx);
    let request = heycode_agent::SubagentRequest::with_authority(
        "isolated",
        "edit safely",
        heycode_agent::SubagentSeed::Fresh,
        heycode_agent::SubagentContinuation::Continuable,
        authority.clone(),
    )
    .unwrap()
    .with_isolation(heycode_agent::ChildIsolation::Worktree);
    let result = registry
        .start(request, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let child = registry.native_child_for(&authority, &result.id).unwrap();
    let path = child.cwd().to_path_buf();
    assert_ne!(path, repository);
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "child\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("seed.txt")).unwrap(),
        "untracked input\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("shell.txt")).unwrap(),
        "child"
    );
    assert_eq!(
        child.session().lock().unwrap().metadata().unwrap().cwd(),
        Some(path.as_path())
    );
    let reply = result
        .handle
        .unwrap()
        .send("continue", tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(reply, "followup complete");
    assert!(
        registry
            .close_child_for(
                &authority,
                &result.id,
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(path.join("shell.txt")).unwrap(),
        "childfollowup"
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "dirty\n"
    );
    assert!(!repository.join("shell.txt").exists());
}
#[tokio::test]
async fn native_worktree_keeps_actual_tools_cwd_and_results_across_followup_and_close() {
    native_worktree_lifecycle(false).await;
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_worktree_preserves_workspace_sandbox_mode_for_filesystem_and_shell() {
    native_worktree_lifecycle(true).await;
}

#[tokio::test]
async fn one_shot_native_child_cannot_admit_optional_question_through_code_mode() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(
        vec![
            vec![
                call(
                    "delegate",
                    serde_json::json!({"label":"one-shot child","prompt":"inspect independently","mode":"oneshot"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            vec![
                call_named(
                    "unsupported-question",
                    "run_code",
                    serde_json::json!({"source":"return await tools.ask_user_question_async({question:'Unavailable follow-up?'});", "tools":["ask_user_question_async"]}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child completed without asking"),
            stop_text("parent done"),
        ],
        root.path().to_path_buf(),
        3,
    );
    heycode_agent::code_mode_plugin()
        .apply(&mut context)
        .unwrap();
    let parent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let child_handle = Arc::new(std::sync::Mutex::new(None));
    let captured = child_handle.clone();
    let _observer = registry.attach_native_child_observer(Arc::new(move |_, child| {
        *captured.lock().unwrap() = Some(child);
    }));
    parent.send("delegate").await.unwrap();
    let child = child_handle.lock().unwrap().clone().unwrap();
    assert!(child.async_questions().unwrap().is_empty());
    assert!(parent.async_questions().unwrap().is_empty());
    let session = child.session().lock().unwrap();
    let projection = heycode_session::project_code_mode(session.events()).unwrap();
    assert!(projection.runs.values().all(|run| {
        run.error.is_some()
            || run
                .result
                .as_ref()
                .is_some_and(|result| result.get("question_id").is_none())
    }));
    drop(session);
    context.shutdown();
}

#[tokio::test]
async fn run_code_async_question_is_owned_by_executing_child() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(
        vec![
            vec![
                call(
                    "delegate",
                    serde_json::json!({"label":"question child", "prompt":"ask an optional question", "mode":"continuable"}),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            vec![
                call_named(
                    "child-script",
                    "run_code",
                    serde_json::json!({
                        "source":"return await tools.ask_user_question_async({questions:[{id:'format',question:'Child output format?',mode:'multiple_choice',options:[{label:'JSON',description:'Structured data'},{label:'Text',description:'Plain explanation'}]}]});",
                        "tools":["ask_user_question_async"]
                    }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("child continued without an answer"),
            stop_text("parent done"),
            stop_text("child used the optional answer"),
        ],
        root.path().to_path_buf(),
        3,
    );
    heycode_agent::code_mode_plugin()
        .apply(&mut context)
        .unwrap();
    let parent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let child_handle = Arc::new(std::sync::Mutex::new(None));
    let child_ui = Arc::new(std::sync::Mutex::new(Vec::new()));
    let parent_ui = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = parent_ui.clone();
    parent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    let handle = child_handle.clone();
    let sink = child_ui.clone();
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let _observer = registry.attach_native_child_observer(Arc::new(move |_, child| {
        let sink = sink.clone();
        child
            .ui()
            .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
        *handle.lock().unwrap() = Some(child);
    }));
    let parent_id = parent.session().lock().unwrap().id().to_string();
    parent.send("delegate").await.unwrap();
    assert!(parent.async_questions().unwrap().is_empty());
    assert!(parent.pending_inbox().is_empty());
    let children = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy() != parent_id)
        .filter(|entry| entry.path().join("session-controls.json").exists())
        .collect::<Vec<_>>();
    assert_eq!(
        children.len(),
        1,
        "only the executing child owns the pending question"
    );
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(children[0].path().join("session-controls.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["questions"].as_array().unwrap().len(), 1);
    assert_eq!(state["questions"][0]["question"], "Child output format?");
    let session = heycode_session::Session::open(children[0].path()).unwrap();
    let projection = heycode_session::project_code_mode(session.events()).unwrap();
    let run = projection.runs.values().next().unwrap();
    assert!(run.settled && run.error.is_none(), "{run:?}");
    assert_eq!(
        run.result.as_ref().unwrap()["value"]["question_ids"][0],
        state["questions"][0]["id"]
    );
    assert!(
        heycode_session::derive_messages(session.events())
            .iter()
            .any(|message| message
                .content
                .contains("child continued without an answer"))
    );
    // A fresh Tokio task has no inherited question-owner scope. The workflow
    // entry must restore the captured child's owner, including its UI bus.
    let child = child_handle.lock().unwrap().clone().unwrap();
    let output = tokio::spawn(async move {
        child.execute_workflow_tool(
            "run_code".into(),
            serde_json::json!({
                "source":"return await tools.ask_user_question_async({question:'Detached child preference?'});",
                "tools":["ask_user_question_async"]
            }),
            tokio_util::sync::CancellationToken::new(),
        ).await
    }).await.unwrap().unwrap();
    assert_eq!(output["ok"], true, "{output}");
    let child = child_handle.lock().unwrap().clone().unwrap();
    assert_eq!(child.async_questions().unwrap().len(), 2);
    assert!(parent.async_questions().unwrap().is_empty());
    let is_detached_question = |event: &heycode_agent::UiEvent| {
        matches!(event,
        heycode_agent::UiEvent::OptionalQuestionRequested { prompt, .. } if prompt == "Detached child preference?")
    };
    assert!(child_ui.lock().unwrap().iter().any(is_detached_question));
    assert!(!parent_ui.lock().unwrap().iter().any(is_detached_question));
    let authority = registry.root_authority(heycode_agent::SubagentId::new(parent_id).unwrap());
    let task = registry
        .task_snapshots_for(&authority)
        .into_iter()
        .next()
        .unwrap();
    let task_id = heycode_agent::SubagentId::new(task.id).unwrap();
    let questions = child.async_questions().unwrap();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let foreign = registry.root_authority(heycode_agent::SubagentId::new("foreign-owner").unwrap());
    assert!(
        registry
            .resolve_optional_question_value_for(
                &foreign,
                &task_id,
                questions[0].id.as_str(),
                Some(&heycode_agent::QuestionAnswer::Selected(vec![
                    "JSON".into(),
                    "Text".into()
                ])),
                &jobs
            )
            .is_err()
    );
    assert!(child.pending_inbox().is_empty());
    assert!(
        registry
            .resolve_optional_question_value_for(
                &authority,
                &task_id,
                questions[0].id.as_str(),
                Some(&heycode_agent::QuestionAnswer::Selected(vec![
                    "invalid".into()
                ])),
                &jobs
            )
            .is_err()
    );
    assert!(child.pending_inbox().is_empty());
    registry
        .resolve_optional_question_value_for(
            &authority,
            &task_id,
            questions[0].id.as_str(),
            Some(&heycode_agent::QuestionAnswer::Selected(vec![
                "JSON".into(),
                "Text".into(),
            ])),
            &jobs,
        )
        .unwrap();
    assert!(
        registry
            .resolve_optional_question_value_for(
                &authority,
                &task_id,
                questions[0].id.as_str(),
                Some(&heycode_agent::QuestionAnswer::Selected(vec![
                    "Text".into()
                ])),
                &jobs
            )
            .is_err()
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let done = heycode_session::derive_messages(child.session().lock().unwrap().events())
                .iter()
                .any(|message| message.content == "child used the optional answer");
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let messages = heycode_session::derive_messages(child.session().lock().unwrap().events());
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.content.contains("Answer to optional question"))
            .count(),
        1
    );
    assert!(parent.pending_inbox().is_empty());
    assert!(
        !heycode_session::derive_messages(parent.session().lock().unwrap().events())
            .iter()
            .any(|message| message.content.contains("Answer to optional question"))
    );
    registry
        .resolve_optional_question_value_for(
            &authority,
            &task_id,
            questions[1].id.as_str(),
            None,
            &jobs,
        )
        .unwrap();
    assert!(child.async_questions().unwrap().is_empty());
    context.shutdown();
}

#[tokio::test]
async fn foreground_native_batch_overlaps_and_commits_in_model_order() {
    struct ParallelNative {
        starts: Arc<std::sync::atomic::AtomicUsize>,
        release: tokio_util::sync::CancellationToken,
    }
    impl Provider for ParallelNative {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "parallel-native".into(),
                default_model: "m".into(),
            }
        }
        fn stream(&self, request: ChatRequest) -> ChunkStream {
            let child = request.messages.iter().any(|message| {
                message.role == heycode_llm::Role::User && message.content.starts_with("child-")
            });
            if child {
                self.starts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let release = self.release.clone();
                return Box::pin(futures::stream::unfold(Some(release), |state| async move {
                    let release = state?;
                    release.cancelled().await;
                    Some((
                        Ok(StreamChunk::Finish(heycode_llm::FinishReason::Stop)),
                        None,
                    ))
                }));
            }
            let scripts = if request
                .messages
                .iter()
                .any(|message| message.role == heycode_llm::Role::Tool)
            {
                stop_text("all complete")
            } else {
                let mut scripts = (0..3).map(|index| StreamChunk::ToolCallDelta {
                    index,
                    id: Some(format!("parallel-{index}")),
                    name: Some("task".into()),
                    arguments_delta: serde_json::json!({"label": format!("child-{index}"), "prompt": format!("child-{index}")}).to_string(),
                }).collect::<Vec<_>>();
                scripts.push(StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls));
                scripts
            };
            Box::pin(futures::stream::iter(scripts.into_iter().map(Ok)))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(vec![], root.path().to_path_buf(), 3);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release = tokio_util::sync::CancellationToken::new();
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(ParallelNative {
            starts: starts.clone(),
            release: release.clone(),
        }))
        .unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.set_inference_route("parallel-native", "m", None);
    let running = agent.clone();
    let turn = tokio::spawn(async move { running.send("delegate three").await });
    until(|| starts.load(std::sync::atomic::Ordering::SeqCst) == 3).await;
    let (registry, authority) = native_owner(&ctx);
    assert_eq!(
        registry
            .task_snapshots_for(&authority)
            .iter()
            .filter(|row| row.state == heycode_agent::TaskState::Running)
            .count(),
        3
    );
    release.cancel();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .text,
        "all complete"
    );
    let ids = agent
        .session()
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            heycode_session::SessionEventKind::ToolResult {
                call_id,
                is_error: false,
                ..
            } => Some(call_id.as_str().to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["parallel-0", "parallel-1", "parallel-2"]);
    ctx.shutdown();
}

#[tokio::test]
async fn fork_continuable_preserves_verified_parent_prefix_and_child_followups() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(
        vec![
            stop_text("remembered"),
            stop_text("first child answer"),
            stop_text("second child answer"),
        ],
        root.path().to_path_buf(),
        3,
    );
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("parent-secret-482").await.unwrap();
    let (registry, authority) = native_owner(&ctx);
    let started = registry
        .start(
            heycode_agent::SubagentRequest::with_authority(
                "fork keeper",
                "first child prompt",
                heycode_agent::SubagentSeed::ForkParent,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let child = registry.native_child_for(&authority, &started.id).unwrap();
    assert_eq!(
        started
            .handle
            .unwrap()
            .send(
                "second child prompt",
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .unwrap(),
        "second child answer"
    );
    let child_session = child.session().lock().unwrap();
    assert!(child_session.lineage().is_some());
    for expected in [
        "parent-secret-482",
        "first child prompt",
        "second child prompt",
    ] {
        assert_eq!(child_session.events().iter().filter(|event| matches!(&event.kind, heycode_session::SessionEventKind::UserMessage { text } if text == expected)).count(), 1);
    }
    drop(child_session);
    ctx.shutdown();
}

#[tokio::test]
async fn native_message_receipts_are_durable_and_busy_and_idle_steer_are_consumed_once() {
    use heycode_session::InboxDelivery;
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(vec![], root.path().to_path_buf(), 3);
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = tokio_util::sync::CancellationToken::new();
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(HeldNativeProvider {
            requests: requests.clone(),
            release: release.clone(),
        }))
        .unwrap();
    agent.set_inference_route("held-native", "m", None);
    let (registry, authority) = native_owner(&ctx);
    let (task, initial) = registry
        .start_background_task(
            heycode_agent::SubagentRequest::with_authority(
                "message recipient",
                "held first prompt",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    until(|| requests.lock().unwrap().len() == 1).await;
    let child = registry.native_child_for(&authority, &task).unwrap();
    let steer = registry
        .queue_message(
            &authority,
            &task,
            "busy steering correction".into(),
            true,
            InboxDelivery::Inject,
        )
        .unwrap();
    let follow = registry
        .queue_message(
            &authority,
            &task,
            "queued followup".into(),
            false,
            InboxDelivery::Inject,
        )
        .unwrap();
    let cancelled = registry
        .queue_message(
            &authority,
            &task,
            "cancel this queued message".into(),
            false,
            InboxDelivery::Inject,
        )
        .unwrap();
    assert!(steer.message_id.is_some());
    assert!(follow.message_id.is_some());
    assert_eq!(child.pending_inbox().next_step, 1);
    assert_eq!(child.pending_inbox().next_turn, 2);
    assert!(
        child.token().is_turn_active(),
        "steer must preserve the active child turn"
    );
    let persisted = std::fs::read_to_string(child.session().lock().unwrap().path()).unwrap();
    assert!(persisted.contains(steer.message_id.as_ref().unwrap().as_str()));
    assert!(persisted.contains(follow.message_id.as_ref().unwrap().as_str()));
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert!(jobs.cancel(&cancelled.job_id));
    assert_eq!(
        wait_for_job(&jobs, cancelled.job_id.as_str()).await,
        heycode_agent::JobState::Settled(heycode_agent::JobOutcome::Cancelled)
    );
    assert_eq!(child.pending_inbox().next_turn, 1);
    release.cancel();
    for job in [&initial, &steer.job_id, &follow.job_id] {
        assert_eq!(
            wait_for_job(&jobs, job.as_str()).await,
            heycode_agent::JobState::Settled(heycode_agent::JobOutcome::Completed)
        );
    }
    let idle = registry
        .queue_message(
            &authority,
            &task,
            "idle steering correction".into(),
            true,
            InboxDelivery::Inject,
        )
        .unwrap();
    assert_eq!(
        wait_for_job(&jobs, idle.job_id.as_str()).await,
        heycode_agent::JobState::Settled(heycode_agent::JobOutcome::Completed)
    );
    let session = child.session().lock().unwrap();
    for expected in [
        "held first prompt",
        "busy steering correction",
        "queued followup",
        "idle steering correction",
    ] {
        assert_eq!(session.events().iter().filter(|event| matches!(&event.kind, heycode_session::SessionEventKind::UserMessage { text } if text == expected)).count(), 1, "{expected}");
    }
    assert!(!session.events().iter().any(|event| matches!(&event.kind, heycode_session::SessionEventKind::UserMessage { text } if text == "cancel this queued message")));
    drop(session);
    assert_eq!(child.pending_inbox().next_turn, 0);
    assert_eq!(child.pending_inbox().next_step, 0);
    ctx.shutdown();
}

#[tokio::test]
async fn close_joins_an_active_followup_and_stale_handles_cannot_reopen_it() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(vec![stop_text("ready")], root.path().to_path_buf(), 3);
    let (registry, authority) = native_owner(&ctx);
    let started = registry
        .start(
            heycode_agent::SubagentRequest::with_authority(
                "close recipient",
                "first",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let child = registry.native_child_for(&authority, &started.id).unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(HeldNativeProvider {
            requests: requests.clone(),
            release: tokio_util::sync::CancellationToken::new(),
        }))
        .unwrap();
    child.set_inference_route("held-native", "m", None);
    let handle = started.handle.unwrap();
    let running = handle.clone();
    let turn = tokio::spawn(async move {
        running
            .send("held followup", tokio_util::sync::CancellationToken::new())
            .await
    });
    until(|| requests.lock().unwrap().len() == 1).await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            registry.close_child_for(
                &authority,
                &started.id,
                tokio_util::sync::CancellationToken::new()
            )
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert!(turn.await.unwrap().is_err());
    assert!(!child.token().is_turn_active());
    assert!(
        handle
            .send(
                "must never restart",
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(
        registry.task_snapshots_for(&authority)[0].state,
        heycode_agent::TaskState::Closed
    );
    ctx.shutdown();
}

#[tokio::test]
async fn nested_background_completion_resumes_its_native_parent_without_a_ui_driver() {
    struct NestedWakeProvider {
        release: tokio_util::sync::CancellationToken,
        requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    }
    impl Provider for NestedWakeProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "nested-wake".into(),
                default_model: "m".into(),
            }
        }
        fn stream(&self, request: ChatRequest) -> ChunkStream {
            use futures::StreamExt;
            self.requests.lock().unwrap().push(request.clone());
            let leaf = request.messages.iter().any(|message| {
                message.role == heycode_llm::Role::User && message.content == "held-grandchild"
            });
            if leaf {
                let release = self.release.clone();
                return Box::pin(
                    futures::stream::once(async move {
                        release.cancelled().await;
                        Ok(StreamChunk::TextDelta("grandchild result".into()))
                    })
                    .chain(futures::stream::iter([Ok(StreamChunk::Finish(
                        heycode_llm::FinishReason::Stop,
                    ))])),
                );
            }
            let scripts = if request.messages.iter().any(|message| {
                message.role == heycode_llm::Role::User
                    && message.content.contains("grandchild result")
            }) {
                stop_text("consumed nested result")
            } else if request
                .messages
                .iter()
                .any(|message| message.role == heycode_llm::Role::Tool)
            {
                stop_text("parent idle")
            } else {
                vec![
                    call(
                        "launch-leaf",
                        serde_json::json!({"label":"leaf", "prompt":"held-grandchild", "background":true}),
                    ),
                    StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
                ]
            };
            Box::pin(futures::stream::iter(scripts.into_iter().map(Ok)))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(vec![], root.path().to_path_buf(), 4);
    let release = tokio_util::sync::CancellationToken::new();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(NestedWakeProvider {
            release: release.clone(),
            requests: requests.clone(),
        }))
        .unwrap();
    let root_agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    root_agent.set_inference_route("nested-wake", "spawn-time-model", None);
    let (registry, authority) = native_owner(&ctx);
    let parent = registry
        .start(
            heycode_agent::SubagentRequest::with_authority(
                "native parent",
                "launch a child",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(parent.text, "parent idle");
    let native = registry.native_child_for(&authority, &parent.id).unwrap();
    until(|| {
        requests.lock().unwrap().iter().any(|request| {
            request
                .messages
                .iter()
                .any(|message| message.content == "held-grandchild")
        })
    })
    .await;
    release.cancel();
    until(|| native.session().lock().unwrap().events().iter().any(|event| matches!(&event.kind, heycode_session::SessionEventKind::AssistantMessage { content, .. } if content == "consumed nested result"))).await;
    assert_eq!(
        root_agent.pending_inbox().next_turn,
        0,
        "nested results belong only to the native parent"
    );
    assert_eq!(native.pending_inbox().next_turn, 0);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.model == "spawn-time-model")
    );
    ctx.shutdown();
}

#[tokio::test]
async fn zero_native_lifetime_budget_is_unlimited() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world_ordered_budget(
        vec![],
        root.path().to_path_buf(),
        3,
        PlanOrder::AfterSubagent,
        heycode_agent::SubagentBudgetLimits {
            max_requests: 0,
            ..heycode_agent::SubagentBudgetLimits::default()
        },
    );
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = tokio_util::sync::CancellationToken::new();
    release.cancel();
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(HeldNativeProvider {
            requests: requests.clone(),
            release,
        }))
        .unwrap();
    ctx.get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap()
        .set_inference_route("held-native", "m", None);
    let (registry, authority) = native_owner(&ctx);
    let result = registry
        .start(
            heycode_agent::SubagentRequest::with_authority(
                "unlimited",
                "infer normally",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
                authority,
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(result.is_ok());
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(registry.budget_snapshot().requests_reserved, 1);
    assert_eq!(registry.budget_snapshot().limits.max_requests, 0);
    ctx.shutdown();
}

#[tokio::test]
async fn canonical_agent_controls_preserve_legacy_dispatch_and_never_cancel_on_invalid_or_wait() {
    let root = tempfile::tempdir().unwrap();
    let ctx = world(vec![], root.path().to_path_buf(), 4);
    let parent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = tokio_util::sync::CancellationToken::new();
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(HeldNativeProvider {
            requests: requests.clone(),
            release: release.clone(),
        }))
        .unwrap();
    parent.set_inference_route("held-native", "preserved-model", None);
    let (registry, authority) = native_owner(&ctx);
    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let control = tools.get("agent_control").unwrap();
    assert_eq!(
        control.spec().parameters["required"],
        serde_json::json!(["action"])
    );
    for name in ["list_agents", "list_tasks", "interrupt_task"] {
        assert!(tools.get(name).is_some());
        assert!(
            !tools
                .advertised_names()
                .iter()
                .any(|advertised| advertised == name)
        );
    }
    assert!(
        tools
            .advertised_names()
            .contains(&"agent_control".to_owned())
    );
    let runtime = parent.delegated_runtime_configuration(None, None).unwrap();
    assert!(
        runtime
            .tools()
            .iter()
            .any(|tool| tool.name == "agent_control")
    );
    assert!(!runtime.tools().iter().any(|tool| matches!(
        tool.name.as_str(),
        "list_agents" | "interrupt_task" | "ask_user_question_async"
    )));
    let prompt = runtime.system_prompt().unwrap();
    assert!(prompt.contains("deliver new findings automatically"));
    assert!(
        tools
            .advertised_names()
            .contains(&"send_message".to_owned())
    );
    assert_eq!(
        control.spec().parameters["properties"]["action"]["enum"],
        serde_json::json!(["interrupt", "archive", "restore"])
    );
    assert!(!prompt.contains("ask_user_question_async"));
    assert!(prompt.contains("use ask_user_question instead of burying the question in prose"));
    assert!(prompt.contains("single_choice, multiple_choice, or free_text"));
    assert!(prompt.contains("Never treat a default selection, silence, cancellation"));
    let mut admitted = Vec::new();
    for label in ["first", "second"] {
        admitted.push(
            registry
                .start_background_task(
                    heycode_agent::SubagentRequest::with_authority(
                        label,
                        "held request",
                        heycode_agent::SubagentSeed::Fresh,
                        heycode_agent::SubagentContinuation::Continuable,
                        authority.clone(),
                    )
                    .unwrap(),
                    heycode_session::InboxDelivery::Inject,
                )
                .unwrap(),
        );
    }
    until(|| requests.lock().unwrap().len() == 2).await;
    let cx = heycode_tools::ToolCtx::default();
    for invalid in [
        serde_json::json!({"agent_id":admitted[0].0.as_str()}),
        serde_json::json!({"action":"unknown","agent_id":admitted[0].0.as_str()}),
        serde_json::json!({"action":"interrupt","agent_id":admitted[0].0.as_str(),"unexpected":true}),
        serde_json::json!({"action":"wait","targets":[],"timeout_ms":0}),
        serde_json::json!({"action":"wait","targets":[{"agent_id":admitted[0].0.as_str()}],"timeout_ms":60001}),
    ] {
        assert!(control.run(invalid, &cx).await.is_err());
    }
    let before = registry.task_snapshots_for(&authority);
    assert!(before.iter().all(|task| matches!(
        task.state,
        heycode_agent::TaskState::Queued | heycode_agent::TaskState::Running
    )));
    let targets = before
        .iter()
        .map(|task| serde_json::json!({"agent_id":task.id,"after_revision":task.revision}))
        .collect::<Vec<_>>();
    let unchanged = control
        .run(
            serde_json::json!({"action":"wait","targets":targets,"timeout_ms":0}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(unchanged["changed"], false);
    assert_eq!(unchanged["agents"].as_array().unwrap().len(), 2);
    let cancelled_wait = heycode_tools::ToolCtx::default();
    cancelled_wait.cancellation.cancel();
    assert!(
        control
            .run(
                serde_json::json!({"action":"wait","targets":targets}),
                &cancelled_wait
            )
            .await
            .is_err()
    );
    assert!(
        registry
            .task_snapshots_for(&authority)
            .iter()
            .all(|task| matches!(
                task.state,
                heycode_agent::TaskState::Queued | heycode_agent::TaskState::Running
            ))
    );
    let foreign = registry.root_authority(heycode_agent::SubagentId::new("foreign").unwrap());
    assert!(
        registry
            .wait_tasks_for(
                &foreign,
                &[(admitted[0].0.clone(), 0)],
                std::time::Duration::ZERO,
                cx.cancellation.clone()
            )
            .await
            .is_err()
    );
    let receipt = control
        .run(
            serde_json::json!({"action":"interrupt","agent_id":admitted[0].0.as_str()}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(receipt["status"], "requested");
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        jobs.wait_for_settlement(&admitted[0].1).await.unwrap(),
        heycode_agent::JobOutcome::Cancelled
    );
    let changed = control
        .run(
            serde_json::json!({"action":"wait","targets":targets,"timeout_ms":0}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(changed["changed"], true);
    for action in ["archive", "restore"] {
        assert_eq!(
            control
                .run(
                    serde_json::json!({"action":action,"agent_id":admitted[0].0.as_str()}),
                    &cx
                )
                .await
                .unwrap()["status"],
            "applied"
        );
    }
    let revisions = registry
        .task_snapshots_for(&authority)
        .into_iter()
        .map(|task| serde_json::json!({"agent_id":task.id,"after_revision":task.revision}))
        .collect::<Vec<_>>();
    let wait = control.run(
        serde_json::json!({"action":"wait","targets":revisions,"timeout_ms":1000}),
        &cx,
    );
    let complete_other = async {
        tokio::task::yield_now().await;
        release.cancel();
    };
    let (changed, ()) = tokio::join!(wait, complete_other);
    assert_eq!(
        changed.unwrap()["changed"],
        true,
        "one changed target wakes the entire batch"
    );
    jobs.wait_for_settlement(&admitted[1].1).await.unwrap();
    let sent = control.run(serde_json::json!({"action":"send","agent_id":admitted[0].0.as_str(),"message":"continue"}), &cx).await.unwrap();
    assert_eq!(sent["action"], "send");
    let legacy = tools
        .get("list_tasks")
        .unwrap()
        .run(serde_json::json!({}), &cx)
        .await
        .unwrap();
    assert_eq!(legacy["tasks"].as_array().unwrap().len(), 2);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.model == "preserved-model")
    );
}

#[tokio::test]
async fn named_async_message_has_provenance_and_no_consumption_reply() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(vec![], root.path().to_path_buf(), 4);
    let parent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = tokio_util::sync::CancellationToken::new();
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(HeldNativeProvider {
            requests: requests.clone(),
            release: release.clone(),
        }))
        .unwrap();
    parent.set_inference_route("held-native", "message-model", None);
    let (registry, authority) = native_owner(&ctx);
    let (id, initial_job) = registry
        .start_background_task(
            heycode_agent::SubagentRequest::with_authority(
                "Atlas",
                "held child",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    until(|| requests.lock().unwrap().len() == 1).await;
    let child = registry.native_child_for(&authority, &id).unwrap();
    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let receipt = tools
        .get("send_message")
        .unwrap()
        .run(
            serde_json::json!({"to":"Atlas","message":"Check the second path too"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(receipt["status"], "queued");
    assert_eq!(receipt["agent_id"], id.as_str());
    {
        let session = child.session().lock().unwrap();
        let message = session
            .inbox()
            .next_step()
            .iter()
            .find(|m| m.id().as_str() == receipt["message_id"].as_str().unwrap())
            .unwrap();
        assert!(
            matches!(message.source(),heycode_session::InboxSource::Agent{agent_name,recipient_id,completion_id:None,..} if agent_name=="main" && recipient_id==id.as_str())
        );
        assert!(message.text().contains("Check the second path too"));
    }
    release.cancel();
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    until(|| {
        jobs.list()
            .iter()
            .all(|job| matches!(job.state, heycode_agent::JobState::Settled(_)))
    })
    .await;
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "only the child initial and steered steps should invoke the model"
    );
    assert!(!parent.token().is_turn_active());
    let session = parent.session().lock().unwrap();
    let inbox = session.inbox();
    assert_eq!(
        inbox.next_step().len(),
        1,
        "only the original completion is retained, not a message-consumed receipt"
    );
    assert!(
        matches!(inbox.next_step()[0].source(),heycode_session::InboxSource::Agent{completion_id:Some(completion),..} if completion.ends_with(initial_job.as_str()))
    );
    drop(session);
    ctx.shutdown();
}

#[tokio::test]
async fn explicit_failed_native_retry_preserves_error_and_creates_another_run() {
    struct RetryProvider;
    impl Provider for RetryProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "retry-fixture".into(),
                default_model: "m".into(),
            }
        }
        fn stream(&self, request: ChatRequest) -> ChunkStream {
            let last = request
                .messages
                .iter()
                .rev()
                .find(|m| m.role == heycode_llm::Role::User)
                .map(|m| m.content.as_str())
                .unwrap_or("");
            if last.contains("TRIGGER_RETAINED_FAILURE") {
                return Box::pin(futures::stream::iter(vec![Err(
                    heycode_llm::LlmError::InvalidResponse("RETAINED_PROVIDER_DIAGNOSTIC".into()),
                )]));
            }
            Box::pin(futures::stream::iter(
                stop_text("retained conversation recovered")
                    .into_iter()
                    .map(Ok),
            ))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(vec![], root.path().to_path_buf(), 3);
    ctx.get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(RetryProvider))
        .unwrap();
    ctx.get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap()
        .set_inference_route("retry-fixture", "m", None);
    let (registry, authority) = native_owner(&ctx);
    let started = registry
        .start(
            heycode_agent::SubagentRequest::with_authority(
                "Atlas",
                "retain this conversation",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!registry.retry_available_for(&authority, &started.id));
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let failed_job = registry
        .send_background(
            &authority,
            &started.id,
            "TRIGGER_RETAINED_FAILURE".into(),
            false,
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    assert_eq!(
        jobs.wait_for_settlement(&failed_job).await.unwrap(),
        heycode_agent::JobOutcome::Failed
    );
    let failed = registry
        .task_snapshots_for(&authority)
        .into_iter()
        .find(|s| s.id == started.id.as_str())
        .unwrap();
    let diagnostic = failed.terminal_diagnostic.unwrap();
    assert!(diagnostic.message.contains("RETAINED_PROVIDER_DIAGNOSTIC"));
    assert!(registry.retry_available_for(&authority, &started.id));
    let receipt = registry.retry_for(&authority, &started.id).unwrap();
    assert_ne!(receipt.job_id, failed_job);
    assert_eq!(
        jobs.wait_for_settlement(&receipt.job_id).await.unwrap(),
        heycode_agent::JobOutcome::Completed
    );
    let recovered = registry
        .task_snapshots_for(&authority)
        .into_iter()
        .find(|s| s.id == started.id.as_str())
        .unwrap();
    assert_eq!(recovered.state, heycode_agent::TaskState::Idle);
    assert!(recovered.terminal_diagnostic.is_none());
    assert!(recovered.diagnostics.iter().any(|d| d.id == diagnostic.id));
    assert_eq!(recovered.job_id.as_deref(), Some(receipt.job_id.as_str()));
    assert!(!registry.retry_available_for(&authority, &started.id));
    ctx.shutdown();
}

#[tokio::test]
async fn native_token_limit_retains_partial_output_without_claiming_success() {
    let root = tempfile::tempdir().unwrap();
    let mut ctx = world(
        vec![vec![
            StreamChunk::TextDelta("PARTIAL_FINDING".into()),
            StreamChunk::Finish(heycode_llm::FinishReason::Length),
        ]],
        root.path().to_path_buf(),
        3,
    );
    let (registry, authority) = native_owner(&ctx);
    let (id, job) = registry
        .start_background_task(
            heycode_agent::SubagentRequest::with_authority(
                "Limited scout",
                "trace the runtime",
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        jobs.wait_for_settlement(&job).await.unwrap(),
        heycode_agent::JobOutcome::Failed
    );
    let snapshot = registry
        .task_snapshots_for(&authority)
        .into_iter()
        .find(|s| s.id == id.as_str())
        .unwrap();
    assert_eq!(snapshot.state, heycode_agent::TaskState::Failed);
    let diagnostic = snapshot.terminal_diagnostic.unwrap();
    assert_eq!(diagnostic.code.as_deref(), Some("max_tokens"));
    assert_eq!(
        diagnostic.partial_result.as_deref(),
        Some("PARTIAL_FINDING")
    );
    assert!(diagnostic.message.contains("max_tokens"));
    assert!(registry.retry_available_for(&authority, &id));
    ctx.shutdown();
}

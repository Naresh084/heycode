//! Native orchestration effects, authority, durable resume and failure boundaries.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_agent::{AgentOptions, JobRegistry, WorkflowService, native_workflow_plugin};
use heycode_core::{Plugin, ToolSpec, compose};
use heycode_llm::{ChatRequest, ChunkStream, Provider, ProviderInfo, StreamChunk};
use heycode_session::{Session, WorkflowDefinition, WorkflowNodeState, WorkflowState};
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Recording {
    fail_at: Arc<AtomicUsize>,
    inner: heycode_llm::testing::FakeProvider,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}
impl Provider for Recording {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        let count = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        if count >= self.fail_at.load(Ordering::SeqCst) {
            return Box::pin(futures::stream::once(async {
                Err(heycode_llm::LlmError::InvalidResponse(
                    "fixture provider failed".into(),
                ))
            }));
        }
        self.inner.stream(request)
    }
}
struct World {
    fail_at: Arc<AtomicUsize>,
    context: heycode_core::Context,
    workflows: Arc<WorkflowService>,
    jobs: Arc<JobRegistry>,
    tools: Arc<ToolRegistry>,
    session: Arc<Mutex<Session>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}
fn world(root: &std::path::Path, scripts: Vec<Vec<StreamChunk>>) -> World {
    world_in_session(
        root,
        scripts,
        heycode_session::session_plugin(root.to_path_buf()),
    )
}
fn world_in_session(
    root: &std::path::Path,
    scripts: Vec<Vec<StreamChunk>>,
    session: Box<dyn Plugin>,
) -> World {
    let fail_at = Arc::new(AtomicUsize::new(usize::MAX));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session,
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(root.to_path_buf(), Duration::from_secs(30))
                .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            heycode_llm::LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![Arc::new(Recording {
                fail_at: fail_at.clone(),
                inner: heycode_llm::testing::FakeProvider::new(scripts),
                requests: requests.clone(),
            })],
        ),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::AutoApprove)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::subagent_plugin(root.to_path_buf(), 3),
        heycode_agent::agent_options_plugin(AgentOptions::default()),
        heycode_agent::agent_plugin(),
        heycode_agent::subagent_jobs_plugin(),
        native_workflow_plugin(),
        heycode_agent::team_plugin(),
        heycode_agent::work_plugin_with_teams(),
    ];
    let context = compose(&plugins).unwrap();
    World {
        fail_at,
        workflows: context.get(heycode_agent::SERVICE_WORKFLOWS).unwrap(),
        jobs: (*context
            .get::<Arc<JobRegistry>>(heycode_agent::SERVICE_JOBS)
            .unwrap())
        .clone(),
        tools: context.get(heycode_tools::SERVICE_TOOLS).unwrap(),
        session: context.get(heycode_session::SERVICE_SESSION).unwrap(),
        context,
        requests,
    }
}
fn text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.into()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}
fn graph(steps: Value) -> WorkflowDefinition {
    let definition: WorkflowDefinition = serde_json::from_value(json!({"version":2,"name":"fixture","description":"native effects","capabilities":["progress","delay","tool","agent"],"steps":steps})).unwrap();
    definition.validate().unwrap();
    definition
}
async fn settle(
    world: &World,
    started: &heycode_agent::WorkflowStarted,
) -> heycode_agent::JobOutcome {
    tokio::time::timeout(
        Duration::from_secs(5),
        world.jobs.wait_for_settlement(started.job_id()),
    )
    .await
    .unwrap()
    .unwrap()
}

struct Probe {
    calls: Arc<AtomicUsize>,
    barrier: Option<Arc<tokio::sync::Barrier>>,
    joined: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Tool for Probe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "probe".into(),
            description: "fixture effect".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        if args["cancel"].as_bool().unwrap_or(false) {
            cx.cancellation.cancelled().await;
            self.joined.fetch_add(1, Ordering::SeqCst);
            return Err(ToolError::new("fixture cancelled after joining effect"));
        }
        if args["fail"].as_bool().unwrap_or(false)
            || (args["fail_first"].as_bool().unwrap_or(false) && call == 1)
        {
            return Err(ToolError::new("fixture transient failure"));
        }
        self.joined.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"call":call,"input":args}))
    }
}
fn probe(
    world: &World,
    barrier: Option<Arc<tokio::sync::Barrier>>,
) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let joined = Arc::new(AtomicUsize::new(0));
    world
        .tools
        .register_shared(Arc::new(Probe {
            calls: calls.clone(),
            barrier,
            joined: joined.clone(),
        }))
        .unwrap();
    (calls, joined)
}

#[tokio::test]
async fn graph_fans_out_real_tools_and_binds_fan_in_results() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, _) = probe(&world, Some(Arc::new(tokio::sync::Barrier::new(2))));
    let started = world.workflows.start(graph(json!([
        {"id":"left","label":"left","action":{"kind":"tool","name":"probe","arguments":{"side":"left"}}},
        {"id":"right","label":"right","action":{"kind":"tool","name":"probe","arguments":{"side":"right"}}},
        {"id":"join","label":"join","depends_on":["left","right"],"action":{"kind":"emit","value":{"left":{"$ref":"left#/input/side"},"right":{"$ref":"right#/input/side"}}}}
    ]))).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let projection = world.workflows.projection().unwrap();
    assert_eq!(
        projection.get(started.run_id()).unwrap().nodes()["join"].value,
        json!({"left":"left","right":"right"})
    );
}

#[tokio::test]
async fn native_agent_executes_fixture_tool_and_binds_output_into_real_file() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(
        dir.path(),
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("effect".into()),
                    name: Some("probe".into()),
                    arguments_delta: "{}".into(),
                },
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            text("native result"),
        ],
    );
    let (calls, _) = probe(&world, None);
    let target = dir.path().join("result.txt");
    let started = world.workflows.start(graph(json!([
        {"id":"agent","label":"agent","action":{"kind":"agent","prompt":"Use probe then produce the result"}},
        {"id":"write","label":"write","depends_on":["agent"],"action":{"kind":"tool","name":"write","arguments":{"path":target,"content":{"$ref":"agent#"}}}}
    ]))).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(target).unwrap(), "native result");
    assert_eq!(world.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn retries_are_bounded_and_failed_dependencies_do_not_execute() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, _) = probe(&world, None);
    let started = world.workflows.start(graph(json!([
        {"id":"retry","label":"retry","max_attempts":2,"replay_safe":true,"action":{"kind":"tool","name":"probe","arguments":{"fail_first":true}}},
        {"id":"never","label":"never","depends_on":["retry"],"when":false,"action":{"kind":"tool","name":"probe","arguments":{}}}
    ]))).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let run = world
        .workflows
        .projection()
        .unwrap()
        .get(started.run_id())
        .unwrap()
        .clone();
    assert_eq!(run.nodes()["retry"].attempt, 2);
    assert_eq!(run.nodes()["never"].state, WorkflowNodeState::Skipped);
    let failed = world.workflows.start(graph(json!([
        {"id":"failure","label":"failure","max_attempts":2,"replay_safe":true,"action":{"kind":"tool","name":"probe","arguments":{"fail":true}}},
        {"id":"blocked","label":"blocked","depends_on":["failure"],"action":{"kind":"tool","name":"probe","arguments":{}}}
    ]))).unwrap();
    assert_eq!(
        settle(&world, &failed).await,
        heycode_agent::JobOutcome::Failed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(
        !world
            .workflows
            .projection()
            .unwrap()
            .get(failed.run_id())
            .unwrap()
            .nodes()
            .contains_key("blocked")
    );
}

#[tokio::test]
async fn cancellation_joins_admitted_tool_effect_before_job_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, joined) = probe(&world, None);
    let started = world.workflows.start(graph(json!([
        {"id":"cancel","label":"cancel","action":{"kind":"tool","name":"probe","arguments":{"cancel":true}}}
    ]))).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(world.jobs.cancel(started.job_id()));
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Cancelled
    );
    assert_eq!(joined.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pause_save_resume_never_replays_completed_effects_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, _) = probe(&world, None);
    let definition = graph(json!([
        {"id":"effect","label":"effect","action":{"kind":"tool","name":"probe","arguments":{}}},
        {"id":"boundary","label":"boundary","depends_on":["effect"],"action":{"kind":"delay","millis":100,"value":true}},
        {"id":"last","label":"last","depends_on":["boundary"],"action":{"kind":"emit","value":true}}
    ]));
    world.workflows.save(definition.clone()).unwrap();
    let started = world.workflows.run_saved("fixture").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let projection = world.workflows.projection().unwrap();
            if projection
                .get(started.run_id())
                .unwrap()
                .nodes()
                .get("boundary")
                .is_some_and(|node| node.state == WorkflowNodeState::Started)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    world.workflows.pause(started.run_id()).unwrap();
    settle(&world, &started).await;
    assert_eq!(
        world
            .workflows
            .projection()
            .unwrap()
            .get(started.run_id())
            .unwrap()
            .state(),
        WorkflowState::Paused
    );
    let resumed = world.workflows.resume(started.run_id()).unwrap();
    assert_eq!(
        settle(&world, &resumed).await,
        heycode_agent::JobOutcome::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let path = world
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    let reopened = Session::open(&path).unwrap();
    let projection = heycode_session::project_workflows(reopened.events()).unwrap();
    assert_eq!(projection.definitions()["fixture"], definition);
    assert_eq!(
        projection.get(started.run_id()).unwrap().completed_steps(),
        3
    );
}

#[tokio::test]
async fn crash_left_unknown_effect_is_not_replayed_even_if_declared_replay_safe() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, _) = probe(&world, None);
    let id = heycode_session::WorkflowRunId::new("crash").unwrap();
    let definition = graph(
        json!([{ "id":"effect","label":"effect","max_attempts":2,"replay_safe":true,"action":{"kind":"tool","name":"probe","arguments":{}} }]),
    );
    for change in [
        heycode_session::WorkflowChange::start(id.clone(), definition),
        heycode_session::WorkflowChange::Node {
            version: 1,
            run_id: id.clone(),
            step_id: "effect".into(),
            record: heycode_session::WorkflowNodeRecord {
                attempt: 1,
                state: WorkflowNodeState::Started,
                value: Value::Null,
            },
        },
    ] {
        world
            .session
            .lock()
            .unwrap()
            .append(heycode_session::SessionEventKind::WorkflowChange {
                change: Box::new(change),
            })
            .unwrap();
    }
    let started = world.workflows.resume(&id).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Failed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn graph_validation_explains_invalid_dependency_binding_cycle_and_retry() {
    for (steps, expected) in [
        (
            json!([{ "id":"a","label":"a","depends_on":["missing"],"action":{"kind":"emit","value":null}}]),
            "dependencies",
        ),
        (
            json!([{ "id":"a","label":"a","action":{"kind":"emit","value":{"$ref":"missing#"}}}]),
            "declared dependency",
        ),
        (
            json!([{ "id":"a","label":"a","depends_on":["b"],"action":{"kind":"emit","value":null}},{"id":"b","label":"b","depends_on":["a"],"action":{"kind":"emit","value":null}}]),
            "cycle",
        ),
        (
            json!([{ "id":"a","label":"a","max_attempts":2,"action":{"kind":"tool","name":"probe","arguments":{}}}]),
            "replay_safe",
        ),
    ] {
        let definition:WorkflowDefinition=serde_json::from_value(json!({"version":2,"name":"invalid","description":"invalid","capabilities":["progress","tool"],"steps":steps})).unwrap();
        assert!(
            definition
                .validate()
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
    let schema = heycode_agent::workflow_definition_schema();
    assert_eq!(
        schema["properties"]["steps"]["items"]["properties"]["action"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

async fn team_tool(world: &World, args: Value) -> Value {
    world
        .tools
        .get("team")
        .unwrap()
        .run(args, &ToolCtx::default())
        .await
        .unwrap()
}
async fn wait_team_review(world: &World, count: usize) {
    let service = world
        .context
        .get::<heycode_agent::TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = service.projection().unwrap();
            let team = projection
                .team(&heycode_session::TeamId::new("team").unwrap())
                .unwrap();
            if team
                .tasks()
                .iter()
                .filter(|task| task.state() == heycode_session::TeamTaskState::Blocked)
                .count()
                == count
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// The fixture lead reviews a returned result and explicitly completes its work.
async fn approve_team_result(world: &World, task: &str) {
    let id = heycode_session::team_work_id(
        &heycode_session::TeamId::new("team").unwrap(),
        &heycode_session::TeamTaskId::new(task).unwrap(),
    )
    .unwrap();
    let item = world
        .tools
        .get("task_get")
        .unwrap()
        .run(json!({"id":id}), &ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(item["fields"]["status"], "blocked");
    assert!(
        item["result_summary"]
            .as_str()
            .unwrap()
            .contains("awaiting review")
    );
    world
        .tools
        .get("task_update")
        .unwrap()
        .run(
            json!({"id":id,"expected_revision":item["revision"],"status":"completed"}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn team_bootstrap_auto_dispatches_dependencies_and_delivers_mail_once_to_owner() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(
        dir.path(),
        vec![
            text("worker ready"),
            text("reviewer ready"),
            text("implementation result"),
            text("review result"),
            text("mail acknowledged"),
        ],
    );
    let created = team_tool(&world, json!({"action":"create","team_id":"team"})).await;
    assert_eq!(created["members"].as_array().unwrap().len(), 3);
    let worker = created["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "worker")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reviewer = created["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "reviewer")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    team_tool(&world,json!({"action":"create_task","team_id":"team","revision":created["revision"],"task_id":"implement","title":"Implement fixture","assignee":worker})).await;
    // Exact revision is fetched again because automatic completions can advance it.
    wait_team_review(&world, 1).await;
    approve_team_result(&world, "implement").await;
    let current = team_tool(&world, json!({"action":"snapshot","team_id":"team"})).await;
    team_tool(&world,json!({"action":"create_task","team_id":"team","revision":current["revision"],"task_id":"review","title":"Review result","assignee":reviewer,"dependencies":["implement"]})).await;
    wait_team_review(&world, 1).await;
    approve_team_result(&world, "review").await;
    let current = team_tool(&world, json!({"action":"snapshot","team_id":"team"})).await;
    let sent=team_tool(&world,json!({"action":"send_mail","team_id":"team","revision":current["revision"],"message_id":"mail-1","recipient":worker,"message":"peer-specific update"})).await;
    assert_eq!(sent["mail"][0]["delivered"], true);
    assert_eq!(sent["mail"][0]["claimed"], false);
    tokio::time::timeout(Duration::from_secs(5), async {
        while world.requests.lock().unwrap().len() < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let requests = world.requests.lock().unwrap();
    assert!(format!("{:?}", requests[3].messages).contains("implementation result"));
    assert!(format!("{:?}", requests[4].messages).contains("peer-specific update"));
    drop(requests);
    let registry = world
        .context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let owner =
        heycode_agent::SubagentId::new(world.session.lock().unwrap().id().as_str()).unwrap();
    let lead = registry.root_authority(owner);
    let worker_authority = registry
        .authority_for_child(&lead, &heycode_agent::SubagentId::new(&worker).unwrap())
        .unwrap();
    let reviewer_authority = registry
        .authority_for_child(&lead, &heycode_agent::SubagentId::new(reviewer).unwrap())
        .unwrap();
    let service = world
        .context
        .get::<heycode_agent::TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let team = heycode_session::TeamId::new("team").unwrap();
    let mail = heycode_session::TeamMessageId::new("mail-1").unwrap();
    assert!(
        service
            .claim_mail(
                &reviewer_authority,
                &team,
                sent["revision"].as_u64().unwrap(),
                &mail
            )
            .is_err()
    );
    assert_eq!(service.deliver_pending(&lead, &team).unwrap(), 0);
    let revision = service.view(&lead, &team).unwrap().revision();
    assert_eq!(
        service
            .claim_mail(&worker_authority, &team, revision, &mail)
            .unwrap(),
        "peer-specific update"
    );
    let view = service.view(&lead, &team).unwrap();
    assert!(view.mail()[0].1);
    assert!(view.mail()[0].2);
    assert!(
        service
            .claim_mail(&worker_authority, &team, view.revision(), &mail)
            .is_err()
    );
    let child_agent = registry
        .native_child_for(&lead, &heycode_agent::SubagentId::new(&worker).unwrap())
        .unwrap();
    let child = child_agent.session().lock().unwrap();
    let inserts=child.events().iter().filter(|event|matches!(&event.kind,heycode_session::SessionEventKind::AgentInboxSplice{inserted,..} if inserted.iter().any(|message|message.text().contains("peer-specific update")))).count();
    assert_eq!(inserts, 1);
}

#[tokio::test]
async fn cancellation_blocks_team_task_without_replaying_or_releasing_dependants() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(
        dir.path(),
        vec![
            text("ready"),
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("wait".into()),
                    name: Some("probe".into()),
                    arguments_delta: r#"{"cancel":true}"#.into(),
                },
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
        ],
    );
    let (calls, joined) = probe(&world, None);
    let created=team_tool(&world,json!({"action":"create","team_id":"team","roles":[{"display":"Worker","role":"worker","instructions":"Implement tasks"}]})).await;
    let worker = created["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "worker")
        .unwrap()["id"]
        .clone();
    let task=team_tool(&world,json!({"action":"create_task","team_id":"team","revision":created["revision"],"task_id":"wait","title":"Wait","assignee":worker})).await;
    let job = heycode_agent::JobId::parse(task["dispatched_jobs"][0].as_str().unwrap()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let current = team_tool(&world, json!({"action":"snapshot","team_id":"team"})).await;
    team_tool(&world,json!({"action":"create_task","team_id":"team","revision":current["revision"],"task_id":"blocked","title":"Must not run","assignee":worker,"dependencies":["wait"]})).await;
    // Recovery may not relabel a currently owned live job.
    let recovered = team_tool(&world, json!({"action":"recover","team_id":"team"})).await;
    assert_eq!(recovered["recovered"], 0);
    assert!(world.jobs.cancel(&job));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), world.jobs.wait_for_settlement(&job))
            .await
            .unwrap()
            .unwrap(),
        heycode_agent::JobOutcome::Cancelled
    );
    assert_eq!(joined.load(Ordering::SeqCst), 1);
    let current = team_tool(&world, json!({"action":"snapshot","team_id":"team"})).await;
    assert!(
        current["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == "wait" && task["state"] == "blocked")
    );
    assert!(
        current["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == "blocked" && task["state"] == "pending")
    );
    assert_eq!(world.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn graph_executes_live_permission_guard_before_tool_effect() {
    struct DenyProbe;
    #[async_trait::async_trait]
    impl heycode_core::Layer<heycode_tools::PreToolDecision> for DenyProbe {
        async fn handle(
            &self,
            input: &mut heycode_tools::PreToolDecision,
            mut next: heycode_core::Next<'_, heycode_tools::PreToolDecision>,
        ) -> anyhow::Result<()> {
            if input.call.name == "probe" {
                input.verdict = heycode_tools::Verdict::Deny {
                    reason: "fixture policy forbids effect".into(),
                };
                return Ok(());
            }
            next.run(input).await
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, _) = probe(&world, None);
    world
        .context
        .get::<heycode_core::Waterfall<heycode_tools::PreToolDecision>>(
            heycode_tools::SEAM_PRE_TOOL,
        )
        .unwrap()
        .push_shared(DenyProbe);
    let started=world.workflows.start(graph(json!([{ "id":"denied","label":"denied","action":{"kind":"tool","name":"probe","arguments":{}} }]))).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Failed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let projection = world.workflows.projection().unwrap();
    assert!(
        projection.get(started.run_id()).unwrap().nodes()["denied"]
            .value
            .as_str()
            .unwrap()
            .contains("policy forbids")
    );
}

#[tokio::test]
async fn concurrent_team_settlements_release_fan_in_once_with_dependency_results() {
    let dir = tempfile::tempdir().unwrap();
    let tool_call = || {
        vec![
            StreamChunk::ToolCallDelta {
                index: 0,
                id: Some("effect".into()),
                name: Some("probe".into()),
                arguments_delta: "{}".into(),
            },
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ]
    };
    let world = world(
        dir.path(),
        vec![
            text("ready"),
            text("ready"),
            tool_call(),
            tool_call(),
            text("branch result"),
            text("branch result"),
            text("joined result"),
        ],
    );
    let (calls, _) = probe(&world, Some(Arc::new(tokio::sync::Barrier::new(2))));
    let created = team_tool(&world, json!({"action":"create","team_id":"team"})).await;
    let worker = created["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "worker")
        .unwrap()["id"]
        .clone();
    let reviewer = created["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "reviewer")
        .unwrap()["id"]
        .clone();
    let first=team_tool(&world,json!({"action":"create_task","team_id":"team","revision":created["revision"],"task_id":"left","title":"Left branch","assignee":worker,"auto_dispatch":false})).await;
    let second=team_tool(&world,json!({"action":"create_task","team_id":"team","revision":first["revision"],"task_id":"right","title":"Right branch","assignee":reviewer,"auto_dispatch":false})).await;
    team_tool(&world,json!({"action":"create_task","team_id":"team","revision":second["revision"],"task_id":"join","title":"Join branches","assignee":worker,"dependencies":["left","right"],"auto_dispatch":false})).await;
    let dispatch = team_tool(&world, json!({"action":"dispatch_ready","team_id":"team"})).await;
    assert_eq!(dispatch["job_ids"].as_array().unwrap().len(), 2);
    wait_team_review(&world, 2).await;
    approve_team_result(&world, "left").await;
    approve_team_result(&world, "right").await;
    team_tool(&world, json!({"action":"dispatch_ready","team_id":"team"})).await;
    wait_team_review(&world, 1).await;
    approve_team_result(&world, "join").await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(world.requests.lock().unwrap().len(), 7);
    let projection = world
        .context
        .get::<heycode_agent::TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap()
        .projection()
        .unwrap();
    let view = projection
        .team(&heycode_session::TeamId::new("team").unwrap())
        .unwrap();
    assert!(
        view.tasks()
            .iter()
            .all(|task| task.state() == heycode_session::TeamTaskState::Completed)
    );
}

#[tokio::test]
async fn failed_role_bootstrap_never_publishes_a_partial_roster() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), vec![text("first role ready")]);
    world.fail_at.store(2, Ordering::SeqCst);
    let error = world
        .tools
        .get("team")
        .unwrap()
        .run(
            json!({"action":"create","team_id":"team"}),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no new roles were admitted"));
    let service = world
        .context
        .get::<heycode_agent::TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let projection = service.projection().unwrap();
    let view = projection
        .team(&heycode_session::TeamId::new("team").unwrap())
        .unwrap();
    assert_eq!(view.members().len(), 1);
    assert_eq!(view.revision(), 0);
    let registry = world
        .context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let owner =
        heycode_agent::SubagentId::new(world.session.lock().unwrap().id().as_str()).unwrap();
    let authority = registry.root_authority(owner);
    assert!(registry.children_for(&authority).is_empty());
}

#[tokio::test]
async fn child_workflow_executes_under_its_own_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let nested = graph(
        json!([{ "id":"effect","label":"effect","action":{"kind":"tool","name":"probe","arguments":{}} }]),
    );
    let world = world(
        dir.path(),
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("nested".into()),
                    name: Some("workflow".into()),
                    arguments_delta: json!({"action":"start","definition":nested}).to_string(),
                },
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            text("nested request accepted"),
        ],
    );
    let (calls, _) = probe(&world, None);
    let started=world.workflows.start(graph(json!([{ "id":"child","label":"child","action":{"kind":"agent","prompt":"Attempt a nested workflow"} }]))).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Completed
    );
    for job in world.jobs.list() {
        tokio::time::timeout(
            Duration::from_secs(5),
            world.jobs.wait_for_settlement(&job.id),
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(world.workflows.projection().unwrap().iter().count(), 1);
    assert!(format!("{:?}", world.requests.lock().unwrap()[1].messages).contains("run_id"));
}

#[tokio::test]
async fn fork_cannot_resume_ancestor_workflow_effects() {
    let dir = tempfile::tempdir().unwrap();
    let parent = world(dir.path(), Vec::new());
    let id = heycode_session::WorkflowRunId::new("parent-run").unwrap();
    let definition = graph(
        json!([{ "id":"effect","label":"effect","action":{"kind":"tool","name":"probe","arguments":{}} }]),
    );
    parent.workflows.save(definition.clone()).unwrap();
    parent
        .session
        .lock()
        .unwrap()
        .append(heycode_session::SessionEventKind::WorkflowChange {
            change: Box::new(heycode_session::WorkflowChange::start(
                id.clone(),
                definition,
            )),
        })
        .unwrap();
    let fork = parent
        .session
        .lock()
        .unwrap()
        .fork(dir.path(), heycode_session::ForkBoundary::Latest)
        .unwrap();
    let path = fork.path().parent().unwrap().to_path_buf();
    drop(fork);
    let child = world_in_session(
        dir.path(),
        Vec::new(),
        heycode_session::session_resume_plugin(path),
    );
    let (calls, _) = probe(&child, None);
    let projection = child.workflows.projection().unwrap();
    assert_eq!(projection.iter().count(), 0);
    assert!(projection.definitions().is_empty());
    assert_eq!(
        child.workflows.run_saved("fixture").unwrap_err(),
        heycode_agent::WorkflowError::NotFound
    );
    assert_eq!(
        child.workflows.resume(&id).unwrap_err(),
        heycode_agent::WorkflowError::NotFound
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn graph_selects_conditional_branch_and_propagates_skip() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Vec::new());
    let (calls, _) = probe(&world, None);
    let started=world.workflows.start(graph(json!([
        {"id":"choice","label":"choice","action":{"kind":"emit","value":{"status":"accepted"}}},
        {"id":"yes","label":"yes","depends_on":["choice"],"when":{"equals":[{"$ref":"choice#/status"},"accepted"]},"action":{"kind":"tool","name":"probe","arguments":{}}},
        {"id":"no","label":"no","depends_on":["choice"],"when":{"equals":[{"$ref":"choice#/status"},"rejected"]},"action":{"kind":"tool","name":"probe","arguments":{}}},
        {"id":"skip","label":"skip","depends_on":["no"],"action":{"kind":"tool","name":"probe","arguments":{}}}
    ]))).unwrap();
    assert_eq!(
        settle(&world, &started).await,
        heycode_agent::JobOutcome::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let projection = world.workflows.projection().unwrap();
    let run = projection.get(started.run_id()).unwrap();
    assert_eq!(run.nodes()["yes"].state, WorkflowNodeState::Completed);
    assert_eq!(run.nodes()["no"].state, WorkflowNodeState::Skipped);
    assert_eq!(run.nodes()["skip"].state, WorkflowNodeState::Skipped);
}

#[tokio::test]
async fn team_task_without_final_text_still_durably_settles() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), vec![text("ready"), text("")]);
    let created=team_tool(&world,json!({"action":"create","team_id":"team","roles":[{"display":"Worker","role":"worker","instructions":"Perform assigned work"}]})).await;
    let worker = created["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "worker")
        .unwrap()["id"]
        .clone();
    team_tool(&world,json!({"action":"create_task","team_id":"team","revision":created["revision"],"task_id":"empty","title":"No reply required","assignee":worker})).await;
    wait_team_review(&world, 1).await;
    let current = team_tool(&world, json!({"action":"snapshot","team_id":"team"})).await;
    assert_eq!(
        current["tasks"][0]["result_summary"],
        "Worker finished; awaiting review.\n\nWorker returned without a text response."
    );
}

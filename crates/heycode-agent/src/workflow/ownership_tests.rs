//! Actual native child context and per-run admission regression tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::AgentOptions;
use heycode_core::{Plugin, compose};
use heycode_llm::StreamChunk;
use serde_json::{Value, json};
use std::time::Duration;

fn world(root: &std::path::Path, scripts: Vec<Vec<StreamChunk>>) -> heycode_core::Context {
    world_with_provider(
        root,
        Arc::new(heycode_llm::testing::FakeProvider::new(scripts)),
    )
}
pub(super) fn world_with_provider(
    root: &std::path::Path,
    provider: Arc<dyn heycode_llm::Provider>,
) -> heycode_core::Context {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(root.to_path_buf()),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(root.to_path_buf(), Duration::from_secs(30))
                .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            cwd: root.to_path_buf(),
            ..Default::default()
        }),
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            heycode_llm::LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        crate::approval_plugin(Arc::new(crate::AutoApprove)),
        crate::commands_plugin(),
        crate::compactions_plugin(),
        crate::subagent_plugin(root.to_path_buf(), 3),
        crate::agent_options_plugin(AgentOptions {
            cwd: Some(root.to_path_buf()),
            ..Default::default()
        }),
        crate::agent_plugin(),
        crate::subagent_jobs_plugin(),
        native_workflow_plugin(),
        crate::team_plugin(),
    ];
    compose(&plugins).unwrap()
}
fn graph(name: &str, steps: Value) -> WorkflowDefinition {
    serde_json::from_value(json!({"version":2,"name":name,"description":"ownership fixture","capabilities":["progress","tool","agent"],"steps":steps})).unwrap()
}
fn text(value: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(value.into()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}
struct Effect;
#[async_trait::async_trait]
impl Tool for Effect {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "effect".into(),
            description: "write in caller cwd".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn run(&self, _: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        std::fs::write(cx.cwd.join("effect.txt"), "child effect").unwrap();
        Ok(json!("done"))
    }
}
struct ChildCheck {
    workflow: Arc<dyn Tool>,
    jobs: Arc<JobRegistry>,
    registry: Arc<crate::SubagentRegistry>,
    parent_run: WorkflowRunId,
    child_cwd: std::path::PathBuf,
    seen: Arc<Mutex<Option<Arc<Mutex<Session>>>>>,
}
#[async_trait::async_trait]
impl Tool for ChildCheck {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "child_check".into(),
            description: "verify actual child ownership".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn run(&self, _: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let mut execution = EXECUTING_TOOLS.with(Clone::clone);
        *self.seen.lock().unwrap() = Some(execution.session.clone());
        let authority = crate::subagent::scoped_authority().unwrap();
        let owner = authority.owner().to_string();
        assert!(Arc::ptr_eq(
            self.registry
                .agent_for_authority(&authority)
                .unwrap()
                .session(),
            &execution.session
        ));
        // The ambient native Agent has Effect, but this admission context does not.
        execution.tools = Arc::new(heycode_tools::ToolRegistry::new());
        execution.cwd = self.child_cwd.clone();
        EXECUTING_TOOLS.scope(execution.clone(), async {
            assert_eq!(self.workflow.run(json!({"action":"list"}),cx).await.unwrap(),json!([]));
            assert_eq!(self.workflow.run(json!({"action":"saved"}),cx).await.unwrap(),json!({}));
            for args in [json!({"action":"resume","run_id":self.parent_run}), json!({"action":"pause","run_id":self.parent_run}),json!({"action":"run_saved","name":"parent-secret"})] {
                assert!(self.workflow.run(args,cx).await.is_err());
            }
            let forbidden=graph("forbidden",json!([{ "id":"effect","label":"effect","action":{"kind":"tool","name":"effect","arguments":{}} }]));
            let start=self.workflow.run(json!({"action":"start","definition":forbidden}),cx).await.unwrap();
            let id=JobId::parse(start["job_id"].as_str().unwrap()).unwrap();
            assert_eq!(self.jobs.wait_for_settlement(&id).await.unwrap(),JobOutcome::Failed);
            assert!(!self.child_cwd.join("effect.txt").exists());
        }).await;
        execution.tools.register_shared(Arc::new(Effect)).unwrap();
        EXECUTING_TOOLS.scope(execution, async {
            let allowed=graph("child-only",json!([
                {"id":"effect","label":"effect","action":{"kind":"tool","name":"effect","arguments":{}}},
                {"id":"agent","label":"agent","depends_on":["effect"],"action":{"kind":"agent","prompt":"Return grandchild result"}}
            ]));
            self.workflow.run(json!({"action":"save","definition":allowed}),cx).await.unwrap();
            let saved=self.workflow.run(json!({"action":"saved"}),cx).await.unwrap();
            assert!(saved.get("child-only").is_some());
            assert!(saved.get("parent-secret").is_none());
            let start=self.workflow.run(json!({"action":"run_saved","name":"child-only"}),cx).await.unwrap();
            let id=JobId::parse(start["job_id"].as_str().unwrap()).unwrap();
            assert_eq!(self.jobs.wait_for_settlement(&id).await.unwrap(),JobOutcome::Completed);
            let rows=self.registry.task_snapshots_for(&authority);
            assert_eq!(rows.len(),1,"workflow agent step must belong to this child");
            assert_eq!(rows[0].owner,owner);
            assert_eq!(rows[0].output,"grandchild result");
            let status=self.workflow.run(json!({"action":"list"}),cx).await.unwrap();
            assert_eq!(status.as_array().unwrap().len(),2);
            assert!(!status.to_string().contains(self.parent_run.as_str()));
        }).await;
        Ok(json!("checked"))
    }
}
#[tokio::test]
async fn native_child_admission_keeps_tools_cwd_state_lineage_and_inbox_local() {
    let dir = tempfile::tempdir().unwrap();
    let child_dir = tempfile::tempdir().unwrap();
    let context = world(
        dir.path(),
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("check".into()),
                    name: Some("child_check".into()),
                    arguments_delta: "{}".into(),
                },
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            text("grandchild result"),
            text("child result"),
        ],
    );
    let service = context
        .get::<WorkflowService>(crate::SERVICE_WORKFLOWS)
        .unwrap();
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let jobs = (*context
        .get::<Arc<JobRegistry>>(crate::SERVICE_JOBS)
        .unwrap())
    .clone();
    let registry = context
        .get::<crate::SubagentRegistry>(crate::SERVICE_SUBAGENTS)
        .unwrap();
    service.save(graph("parent-secret",json!([{ "id":"secret","label":"secret","action":{"kind":"emit","value":"secret payload"}}]))).unwrap();
    let parent_run = WorkflowRunId::new("parent-private-run").unwrap();
    append_workflow_change(&service.session,WorkflowChange::start(parent_run.clone(),graph("parent-run",json!([{ "id":"secret","label":"secret","action":{"kind":"emit","value":"secret status"}}])))).unwrap();
    let seen = Arc::new(Mutex::new(None));
    let workflow = tools.get("workflow").unwrap();
    assert_eq!(
        workflow.untrusted_content(),
        Some(heycode_core::UntrustedContentBoundary::tool_orchestration())
    );
    tools.register_shared(Arc::new(Effect)).unwrap();
    tools
        .register_shared(Arc::new(ChildCheck {
            workflow,
            jobs: jobs.clone(),
            registry: registry.clone(),
            parent_run,
            child_cwd: child_dir.path().to_path_buf(),
            seen: seen.clone(),
        }))
        .unwrap();
    let started=service.start(graph("outer",json!([{ "id":"child","label":"child","action":{"kind":"agent","prompt":"Call child_check"}}]))).unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(10),
            jobs.wait_for_settlement(started.job_id())
        )
        .await
        .unwrap()
        .unwrap(),
        JobOutcome::Completed
    );
    assert!(!dir.path().join("effect.txt").exists());
    assert_eq!(
        std::fs::read_to_string(child_dir.path().join("effect.txt")).unwrap(),
        "child effect"
    );
    let child = seen.lock().unwrap().clone().unwrap();
    let child = child.lock().unwrap();
    let projection = project_owned_workflows(&child).unwrap();
    assert_eq!(projection.iter().count(), 2);
    assert_eq!(projection.definitions().len(), 1);
    let root = service.session.lock().unwrap();
    let root_projection = project_owned_workflows(&root).unwrap();
    assert_eq!(root_projection.iter().count(), 2);
    assert_eq!(root_projection.definitions().len(), 1);
    // Settlement persists only to the admitting owner's inbox.
    let child_events = serde_json::to_string(child.events()).unwrap();
    let root_events = serde_json::to_string(root.events()).unwrap();
    for (run_id, _) in projection.iter() {
        assert!(!root_events.contains(run_id.as_str()));
    }
    for job in jobs
        .list()
        .into_iter()
        .filter(|job| job.id != *started.job_id())
    {
        assert!(child_events.contains(job.id.as_str()));
        assert!(!root_events.contains(job.id.as_str()));
    }
}

#[tokio::test]
async fn captured_admission_enforces_approval_and_joins_owner_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let context = world(dir.path(), Vec::new());
    let service = context
        .get::<WorkflowService>(crate::SERVICE_WORKFLOWS)
        .unwrap();
    let mut execution = service.agent.tool_execution_context();
    execution.tools.register_shared(Arc::new(Effect)).unwrap();
    execution.approval = Arc::new(crate::DenyAll);
    let effect = graph(
        "denied",
        json!([{ "id":"effect","label":"effect","action":{"kind":"tool","name":"effect","arguments":{}} }]),
    );
    let started = EXECUTING_TOOLS
        .scope(execution.clone(), async { service.start(effect).unwrap() })
        .await;
    assert_eq!(
        service
            .jobs
            .wait_for_settlement(started.job_id())
            .await
            .unwrap(),
        JobOutcome::Failed
    );
    assert!(!dir.path().join("effect.txt").exists());
    execution.approval = Arc::new(crate::AutoApprove);
    execution.cancellation = crate::AgentCancellation::new();
    let shutdown = execution.cancellation.clone();
    let definition:WorkflowDefinition = serde_json::from_value(json!({"version":2,"name":"shutdown","description":"owner lifecycle","capabilities":["progress","delay","tool"],"steps":[
        {"id":"wait","label":"wait","action":{"kind":"delay","millis":10000,"value":null}},
        {"id":"effect","label":"effect","depends_on":["wait"],"action":{"kind":"tool","name":"effect","arguments":{}}}
    ]})).unwrap();
    let started = EXECUTING_TOOLS
        .scope(execution.clone(), async {
            service.start(definition).unwrap()
        })
        .await;
    shutdown.shutdown();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(3),
            service.jobs.wait_for_settlement(started.job_id())
        )
        .await
        .unwrap()
        .unwrap(),
        JobOutcome::Cancelled
    );
    assert!(!dir.path().join("effect.txt").exists());
    let before = service.projection().unwrap().iter().count();
    let closed = graph(
        "closed",
        json!([{ "id":"effect","label":"effect","action":{"kind":"emit","value":null}}]),
    );
    assert_eq!(
        EXECUTING_TOOLS
            .scope(execution, async { service.start(closed).unwrap_err() })
            .await,
        WorkflowError::Unavailable
    );
    assert_eq!(service.projection().unwrap().iter().count(), before);
}

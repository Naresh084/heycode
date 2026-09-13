//! O12 workflow Provider, background lifecycle, cancellation, and checkpoint resume.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_agent::{
    AgentOptions, JobOutcome, SERVICE_WORKFLOWS, SequentialWorkflowWorker, WorkflowService,
    agent_options_plugin, agent_plugin, approval_plugin, commands_plugin, compactions_plugin,
    workflow_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_exec::{LocalShellConfig, local_execution_plugin};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, Provider, llm_plugin, model_catalog_plugin};
use heycode_prompt::prompt_plugin;
use heycode_session::{
    Session, WorkflowAction, WorkflowCapability, WorkflowDefinition, WorkflowOutcome,
    WorkflowState, WorkflowStep, project_workflows, session_plugin,
};
use heycode_tools::tools_plugin;

struct World {
    context: heycode_core::Context,
    session: Arc<std::sync::Mutex<Session>>,
    workflows: Arc<WorkflowService>,
    jobs: Arc<heycode_agent::JobRegistry>,
}

fn world_with_session(session: Box<dyn Plugin>, root: &std::path::Path) -> World {
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(Vec::new()));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session,
        prompt_plugin(),
        local_execution_plugin(
            LocalShellConfig::platform(root.to_path_buf(), Duration::from_secs(30)).unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".to_owned(),
                model: "fake-model".to_owned(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(heycode_agent::AutoApprove)),
        commands_plugin(),
        agent_options_plugin(AgentOptions::default()),
        compactions_plugin(),
        agent_plugin(),
        workflow_plugin(Arc::new(SequentialWorkflowWorker)),
    ];
    let context = compose(&plugins).unwrap();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    World {
        session: context
            .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
            .unwrap(),
        workflows: context.get::<WorkflowService>(SERVICE_WORKFLOWS).unwrap(),
        jobs: (*jobs).clone(),
        context,
    }
}

fn world(root: &std::path::Path) -> World {
    world_with_session(session_plugin(root.to_path_buf()), root)
}

fn definition(delay_ms: u64) -> WorkflowDefinition {
    WorkflowDefinition::new(
        "coherent-vertical",
        "Run deterministic checkpointed work",
        vec![WorkflowCapability::Progress, WorkflowCapability::Delay],
        vec![
            WorkflowStep::new(
                "prepare",
                "prepare",
                WorkflowAction::Emit {
                    value: serde_json::json!({"prepared":true}),
                },
            )
            .unwrap(),
            WorkflowStep::new(
                "settle",
                "settle",
                WorkflowAction::Delay {
                    millis: delay_ms,
                    value: serde_json::json!({"settled":true}),
                },
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

#[tokio::test]
async fn worker_reports_progress_checkpoints_and_terminal_job_once() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    assert!(
        world
            .context
            .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
            .unwrap()
            .names()
            .contains(&"workflow".to_owned())
    );
    let started = world.workflows.start(definition(1)).unwrap();
    assert_eq!(
        world
            .jobs
            .wait_for_settlement(started.job_id())
            .await
            .unwrap(),
        JobOutcome::Completed
    );
    let projection = project_workflows(
        world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events(),
    )
    .unwrap();
    let run = projection.get(started.run_id()).unwrap();
    assert_eq!(run.state(), WorkflowState::Completed);
    assert_eq!(run.completed_steps(), 2);
    assert_eq!(run.checkpoint(), Some(&serde_json::json!({"settled":true})));
}

#[tokio::test]
async fn cancellation_reaches_the_worker_and_commits_cancelled_checkpoint_state() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let started = world.workflows.start(definition(30_000)).unwrap();
    for _ in 0..300 {
        let completed = project_workflows(
            world
                .session
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .events(),
        )
        .unwrap()
        .get(started.run_id())
        .unwrap()
        .completed_steps();
        if completed == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(world.jobs.cancel(started.job_id()));
    assert_eq!(
        world
            .jobs
            .wait_for_settlement(started.job_id())
            .await
            .unwrap(),
        JobOutcome::Cancelled
    );
    let projection = project_workflows(
        world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events(),
    )
    .unwrap();
    let run = projection.get(started.run_id()).unwrap();
    assert_eq!(run.state(), WorkflowState::Cancelled);
    assert_eq!(run.completed_steps(), 1);
    assert!(world.session.lock().unwrap().events().iter().any(|event| matches!(
        &event.kind,
        heycode_session::SessionEventKind::WorkflowChange { change }
            if matches!(change.as_ref(), heycode_session::WorkflowChange::End { outcome: WorkflowOutcome::Cancelled, .. })
    )));
}

#[tokio::test]
async fn resumed_worker_starts_after_the_durable_checkpoint_prefix() {
    let root = tempfile::tempdir().unwrap();
    let mut seeded = Session::create(root.path()).unwrap();
    let run_id = heycode_session::WorkflowRunId::new("resume-workflow").unwrap();
    for change in [
        heycode_session::WorkflowChange::start(run_id.clone(), definition(1)),
        heycode_session::WorkflowChange::progress(run_id.clone(), 1, 1, "prepare").unwrap(),
        heycode_session::WorkflowChange::checkpoint(
            run_id.clone(),
            1,
            serde_json::json!({"prepared":true}),
        )
        .unwrap(),
    ] {
        seeded
            .append(heycode_session::SessionEventKind::WorkflowChange {
                change: Box::new(change),
            })
            .unwrap();
    }
    seeded.flush().unwrap();
    let session_path = seeded.path().parent().unwrap().to_path_buf();
    drop(seeded);

    let world = world_with_session(
        heycode_session::session_resume_plugin(session_path),
        root.path(),
    );
    let started = world.workflows.resume(&run_id).unwrap();
    assert_eq!(
        world
            .jobs
            .wait_for_settlement(started.job_id())
            .await
            .unwrap(),
        JobOutcome::Completed
    );
    let projection = world.workflows.projection().unwrap();
    let run = projection.get(&run_id).unwrap();
    assert_eq!(run.attempt(), 2);
    assert_eq!(run.completed_steps(), 2);
    assert_eq!(run.progress_sequence(), 2);
}

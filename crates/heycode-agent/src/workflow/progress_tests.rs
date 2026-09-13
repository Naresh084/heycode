//! Authoritative phase progress under real native children and held provider responses.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::{WorkflowNodeKind, WorkflowViewState as State};
use futures::StreamExt;
use heycode_llm::{ChatRequest, ChunkStream, FinishReason, Provider, ProviderInfo, StreamChunk};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct HeldProvider {
    entered: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
    permits: Arc<tokio::sync::Semaphore>,
    fail: bool,
}
impl Provider for HeldProvider {
    fn info(&self) -> ProviderInfo {
        heycode_llm::testing::FakeProvider::new(Vec::new()).info()
    }
    fn stream(&self, _: ChatRequest) -> ChunkStream {
        let index = self.entered.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
        let permits = self.permits.clone();
        let fail = self.fail;
        Box::pin(
            futures::stream::once(async move {
                permits.acquire_owned().await.unwrap().forget();
                if fail {
                    return vec![Err(heycode_llm::LlmError::InvalidResponse(
                        "fixture provider failure".into(),
                    ))];
                }
                let mut chunks = Vec::new();
                if index != 1 {
                    chunks.push(Ok(StreamChunk::Usage(heycode_core::TokenUsage {
                        prompt_tokens: 10 + index as u64,
                        completion_tokens: 5 + index as u64,
                    })));
                }
                chunks.push(Ok(StreamChunk::TextDelta(format!("native result {index}"))));
                chunks.push(Ok(StreamChunk::Finish(FinishReason::Stop)));
                chunks
            })
            .flat_map(futures::stream::iter),
        )
    }
}
struct World {
    context: heycode_core::Context,
    service: Arc<WorkflowService>,
    registry: Arc<crate::SubagentRegistry>,
    owner: crate::SubagentAuthority,
    entered: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
    permits: Arc<tokio::sync::Semaphore>,
}
fn world(root: &std::path::Path, fail: bool) -> World {
    let entered = Arc::new(AtomicUsize::new(0));
    let changed = Arc::new(tokio::sync::Notify::new());
    let permits = Arc::new(tokio::sync::Semaphore::new(0));
    let context = super::ownership_tests::world_with_provider(
        root,
        Arc::new(HeldProvider {
            entered: entered.clone(),
            changed: changed.clone(),
            permits: permits.clone(),
            fail,
        }),
    );
    let service = context
        .get::<WorkflowService>(crate::SERVICE_WORKFLOWS)
        .unwrap();
    let registry = context
        .get::<crate::SubagentRegistry>(crate::SERVICE_SUBAGENTS)
        .unwrap();
    let owner = registry.root_authority(
        crate::SubagentId::new(service.session.lock().unwrap().id().as_str()).unwrap(),
    );
    World {
        context,
        service,
        registry,
        owner,
        entered,
        changed,
        permits,
    }
}
fn definition() -> WorkflowDefinition {
    let definition: WorkflowDefinition = serde_json::from_str(include_str!(
        "../../../../docs/examples/workflow-sequence.json"
    ))
    .unwrap();
    definition.validate().unwrap();
    definition
}
async fn entered(world: &World, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let notified = world.changed.notified();
            if world.entered.load(Ordering::SeqCst) >= count {
                break;
            }
            notified.await;
        }
    })
    .await
    .unwrap();
}
async fn settle(world: &World, started: &WorkflowStarted) -> JobOutcome {
    tokio::time::timeout(
        Duration::from_secs(5),
        world.service.jobs.wait_for_settlement(started.job_id()),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn three_phases_publish_three_real_agents_before_response_then_pause_resume_and_reopen_history()
 {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), false);
    let started = world.service.start(definition()).unwrap();
    entered(&world, 3).await;
    let view = world
        .service
        .view_for(&world.owner, started.run_id())
        .unwrap();
    assert_eq!(view.title, "Research and deliver");
    assert_eq!(view.jobs.len(), 1);
    assert_eq!(view.jobs[0].job_id, *started.job_id());
    assert_eq!(view.jobs[0].attempt, 1);
    assert_eq!(view.jobs[0].outcome, None);
    let events = world.service.session.lock().unwrap().events().to_vec();
    let admission=events.iter().position(|event|matches!(&event.kind,SessionEventKind::WorkflowChange{change} if matches!(change.as_ref(),WorkflowChange::Job{run_id,..} if run_id==started.run_id()))).unwrap();
    let work=events.iter().position(|event|matches!(&event.kind,SessionEventKind::WorkflowChange{change} if matches!(change.as_ref(),WorkflowChange::Node{run_id,..} if run_id==started.run_id()))).unwrap();
    assert!(
        admission < work,
        "coordinator association must commit before any worker effect"
    );
    assert_eq!(
        view.phases
            .iter()
            .map(|phase| phase.id.as_str())
            .collect::<Vec<_>>(),
        vec!["discover", "analyze", "deliver"]
    );
    assert_eq!(
        view.phases
            .iter()
            .map(|phase| phase.state)
            .collect::<Vec<_>>(),
        vec![State::Completed, State::Running, State::Queued]
    );
    assert_eq!(
        (
            view.counts.total,
            view.counts.completed,
            view.counts.running,
            view.counts.queued
        ),
        (5, 1, 3, 1)
    );
    assert!(view.controls.can_pause);
    assert!(!view.controls.can_resume);
    assert_eq!(view.phases[1].agents.len(), 3);
    assert!(view.phases[0].agents.is_empty());
    assert!(view.phases[2].agents.is_empty());
    assert_eq!(view.phases[2].nodes[0].kind, WorkflowNodeKind::Tool);
    assert_eq!(view.usage.reported_requests, 0);
    for agent in &view.phases[1].agents {
        assert_eq!(agent.state, crate::TaskState::Running);
        let task = world
            .registry
            .task_snapshots_for(&world.owner)
            .into_iter()
            .find(|task| task.id == agent.task_id)
            .unwrap();
        assert_eq!(task.session_id, agent.session_id);
        assert_ne!(agent.session_id.as_deref(), Some(agent.task_id.as_str()));
        let actual = world
            .registry
            .native_child_for(
                &world.owner,
                &crate::SubagentId::new(&agent.task_id).unwrap(),
            )
            .unwrap();
        assert_eq!(
            actual.session().lock().unwrap().id().as_str(),
            agent.session_id.as_deref().unwrap()
        );
        assert!(agent.summary.is_empty());
        assert!(agent.timing.started_at_ms.is_some());
        assert!(agent.prompt.contains("Do not modify files."));
    }
    // Concurrent readers never take task locks while holding the owner session.
    let reading = world.service.clone();
    let reader = tokio::spawn(async move {
        for _ in 0..64 {
            assert_eq!(reading.views().unwrap().len(), 1);
            tokio::task::yield_now().await;
        }
    });
    world
        .service
        .pause_for(&world.owner, started.run_id())
        .unwrap();
    assert_eq!(
        world.service.view(started.run_id()).unwrap().state,
        State::Pausing
    );
    world.permits.add_permits(3);
    assert_eq!(settle(&world, &started).await, JobOutcome::Completed);
    tokio::time::timeout(Duration::from_secs(5), reader)
        .await
        .unwrap()
        .unwrap();
    let paused = world.service.view(started.run_id()).unwrap();
    assert_eq!(paused.state, State::Paused);
    assert_eq!(paused.jobs[0].outcome, Some(WorkflowOutcome::Paused));
    assert!(paused.jobs[0].settled_at_ms.is_some());
    assert!(paused.controls.can_resume);
    assert_eq!(paused.phases[1].state, State::Completed);
    assert_eq!(
        (
            paused.usage.reported_requests,
            paused.usage.unreported_requests,
            paused.usage.prompt_tokens,
            paused.usage.completion_tokens
        ),
        (2, 1, 22, 12)
    );
    assert!(!dir.path().join("workflow-report.json").exists());
    let resumed = world
        .service
        .resume_for(&world.owner, started.run_id())
        .unwrap();
    assert_eq!(
        settle(&world, &resumed).await,
        JobOutcome::Completed,
        "{:?}",
        world.service.view(started.run_id()).unwrap()
    );
    assert_eq!(
        world.entered.load(Ordering::SeqCst),
        3,
        "resume must not reexecute any agent"
    );
    assert!(
        std::fs::read_to_string(dir.path().join("workflow-report.json"))
            .unwrap()
            .starts_with("native result")
    );
    let completed = world.service.view(started.run_id()).unwrap();
    assert_eq!(completed.state, State::Completed);
    assert_eq!(
        completed
            .jobs
            .iter()
            .map(|job| (&job.job_id, job.attempt, job.outcome))
            .collect::<Vec<_>>(),
        vec![
            (started.job_id(), 1, Some(WorkflowOutcome::Paused)),
            (resumed.job_id(), 2, Some(WorkflowOutcome::Completed))
        ]
    );
    assert_eq!(completed.counts.completed, 5);
    assert!(
        completed
            .phases
            .iter()
            .all(|phase| phase.state == State::Completed)
    );
    assert_eq!(completed.usage, paused.usage);
    assert!(
        completed.phases[1]
            .agents
            .iter()
            .all(|agent| agent.state == crate::TaskState::Completed
                && agent.summary.starts_with("native result")
                && agent.timing.finished_at_ms.is_some())
    );
    let session_weak = Arc::downgrade(&world.service.session);
    let directory = world
        .service
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_owned();
    world.context.shutdown();
    drop(world.service);
    drop(world.context);
    assert!(
        session_weak.upgrade().is_none(),
        "registry observers must not retain the owner session writer"
    );
    assert!(
        world.registry.task_snapshots().is_empty(),
        "history replay must work after the runtime registry is disposed"
    );
    let reopened = Session::open(directory).unwrap();
    let historical = crate::project_workflow_views(&reopened, now_ms())
        .unwrap()
        .remove(0);
    assert_eq!(historical.phases, completed.phases);
    assert_eq!(historical.usage, completed.usage);
    assert_eq!(
        historical.jobs, completed.jobs,
        "UI-driven resume has durable IDs without a provider ToolResult"
    );
    assert_eq!(historical.job_id, None);
    assert!(!historical.controls.can_resume);
    assert!(!historical.controls.can_cancel);
}

#[tokio::test]
async fn foreign_owners_cannot_read_or_control_and_cancellation_settles_real_agents() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), false);
    let started = world.service.start(definition()).unwrap();
    entered(&world, 3).await;
    let foreign = crate::SubagentRegistry::new().root_authority(world.owner.owner().clone());
    assert!(world.service.views_for(&foreign).is_err());
    assert!(world.service.view_for(&foreign, started.run_id()).is_err());
    assert!(world.service.pause_for(&foreign, started.run_id()).is_err());
    assert!(
        world
            .service
            .resume_for(&foreign, started.run_id())
            .is_err()
    );
    assert!(
        world
            .service
            .cancel_for(&foreign, started.run_id())
            .is_err()
    );
    assert!(
        world
            .service
            .cancel_for(&world.owner, started.run_id())
            .unwrap()
    );
    assert_eq!(settle(&world, &started).await, JobOutcome::Cancelled);
    let view = world.service.view(started.run_id()).unwrap();
    assert_eq!(view.state, State::Cancelled);
    assert_eq!(view.jobs[0].outcome, Some(WorkflowOutcome::Cancelled));
    assert!(
        view.phases[1]
            .agents
            .iter()
            .all(|agent| agent.state == crate::TaskState::Cancelled)
    );
    assert_eq!(view.phases[0].state, State::Completed);
    assert_eq!(view.phases[1].state, State::Cancelled);
    assert_eq!(
        view.phases[2].nodes[0].state,
        State::Queued,
        "unexecuted tool is not a fabricated failure or success"
    );
    assert!(!view.controls.can_resume);
    assert!(!dir.path().join("workflow-report.json").exists());
}

#[tokio::test]
async fn provider_failure_retains_failed_agent_summaries_and_blocks_future_phase() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), true);
    let started = world.service.start(definition()).unwrap();
    entered(&world, 3).await;
    world.permits.add_permits(3);
    assert_eq!(settle(&world, &started).await, JobOutcome::Failed);
    let view = world.service.view(started.run_id()).unwrap();
    assert_eq!(view.state, State::Failed);
    assert_eq!(view.jobs[0].outcome, Some(WorkflowOutcome::Failed));
    assert!(
        view.phases[1]
            .agents
            .iter()
            .all(|agent| agent.state == crate::TaskState::Failed
                && agent.summary.contains("fixture provider failure"))
    );
    assert_eq!(view.phases[1].state, State::Failed);
    assert_eq!(view.counts.failed, 3);
    assert_eq!(view.phases[2].nodes[0].state, State::Queued);
    assert_eq!(view.usage.reported_requests, 0);
    assert!(!dir.path().join("workflow-report.json").exists());
}

#[tokio::test]
async fn observation_persistence_failure_prevents_first_provider_request() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), false);
    let request = crate::SubagentRequest::with_authority(
        "observe",
        "Must not infer",
        crate::SubagentSeed::Fresh,
        crate::SubagentContinuation::OneShot,
        world.owner.clone(),
    )
    .unwrap()
    .with_task_observer(Arc::new(crate::task_inventory::TaskObserver::new(|_| {
        Err(std::io::Error::other("fixture observation refused"))
    })));
    assert!(
        world
            .registry
            .start(request, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(world.entered.load(Ordering::SeqCst), 0);
    assert!(world.registry.task_snapshots().is_empty());
}

#[tokio::test]
async fn cancelling_paused_history_does_not_resume_or_execute_pending_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), false);
    let run_id = WorkflowRunId::new("paused-history").unwrap();
    append_workflow_change(
        &world.service.session,
        WorkflowChange::start(run_id.clone(), definition()),
    )
    .unwrap();
    for state in [
        heycode_session::WorkflowNodeState::Started,
        heycode_session::WorkflowNodeState::Completed,
    ] {
        append_workflow_change(
            &world.service.session,
            WorkflowChange::Node {
                version: 1,
                run_id: run_id.clone(),
                step_id: "scope".into(),
                record: heycode_session::WorkflowNodeRecord {
                    attempt: 1,
                    state,
                    value: serde_json::Value::Null,
                },
            },
        )
        .unwrap();
    }
    append_workflow_change(
        &world.service.session,
        WorkflowChange::end(
            run_id.clone(),
            WorkflowOutcome::Paused,
            1,
            Some("paused at safe boundary".into()),
        )
        .unwrap(),
    )
    .unwrap();
    let view = world.service.view_for(&world.owner, &run_id).unwrap();
    assert!(view.controls.can_resume);
    assert!(view.controls.can_cancel);
    assert_eq!(view.phases[0].state, State::Completed);
    assert!(world.service.cancel_for(&world.owner, &run_id).unwrap());
    let stopped = world.service.view(&run_id).unwrap();
    assert_eq!(stopped.state, State::Cancelled);
    assert_eq!(stopped.attempt, 1);
    assert_eq!(stopped.counts.completed, 1);
    assert_eq!(world.entered.load(Ordering::SeqCst), 0);
    assert!(world.service.jobs.list().is_empty());
}

#[tokio::test]
async fn interrupted_unknown_effects_have_no_invented_elapsed_or_resume_permission() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), false);
    let run_id = WorkflowRunId::new("unknown-history").unwrap();
    append_workflow_change(
        &world.service.session,
        WorkflowChange::start(run_id.clone(), definition()),
    )
    .unwrap();
    append_workflow_change(
        &world.service.session,
        WorkflowChange::Node {
            version: 1,
            run_id: run_id.clone(),
            step_id: "scope".into(),
            record: heycode_session::WorkflowNodeRecord {
                attempt: 1,
                state: heycode_session::WorkflowNodeState::Started,
                value: serde_json::Value::Null,
            },
        },
    )
    .unwrap();
    let view = world.service.view(&run_id).unwrap();
    assert_eq!(view.state, State::Interrupted);
    assert!(!view.controls.can_resume);
    assert_eq!(view.timing.elapsed_ms, None);
    assert_eq!(view.phases[0].timing.elapsed_ms, None);
    assert_eq!(view.phases[0].nodes[0].timing.elapsed_ms, None);
    assert!(world.service.cancel_for(&world.owner, &run_id).unwrap());
    let cancelled = world.service.view(&run_id).unwrap();
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(
        cancelled.phases[0].nodes[0].state,
        State::Interrupted,
        "cancelling recovery cannot invent the unknown effect outcome"
    );
}

#[tokio::test]
async fn coordinator_admission_write_failure_settles_failed_without_polling_native_work() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), false);
    let run_id = WorkflowRunId::new("admission-failure").unwrap();
    let definition = definition();
    append_workflow_change(
        &world.service.session,
        WorkflowChange::start(run_id.clone(), definition.clone()),
    )
    .unwrap();
    flush_workflow_session(&world.service.session).unwrap();
    let path = world
        .service
        .session
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_owned();
    // Supersede only after the run Start exists; the coordinator's Job append
    // must now fail at the real session write boundary.
    let authoritative = Session::open_for_writing(&path).unwrap();
    let started = world
        .service
        .spawn_attempt(
            run_id,
            definition,
            0,
            1,
            world.service.agent.tool_execution_context(),
        )
        .unwrap();
    assert_eq!(settle(&world, &started).await, JobOutcome::Failed);
    assert_eq!(world.entered.load(Ordering::SeqCst), 0);
    assert!(world.registry.task_snapshots().is_empty());
    assert!(!dir.path().join("workflow-report.json").exists());
    assert!(world.service.runtime.lock().unwrap().active.is_empty());
    let projection = heycode_session::project_workflows(authoritative.events()).unwrap();
    let run = projection.get(started.run_id()).unwrap();
    assert!(run.jobs().is_empty());
    assert!(run.nodes().is_empty());
}

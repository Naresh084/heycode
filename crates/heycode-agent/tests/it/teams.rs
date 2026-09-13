#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, AutoApprove, SubagentCapabilities, SubagentContinuation, SubagentError,
    SubagentHandle, SubagentId, SubagentProvider, SubagentProviderDescriptor, SubagentRegistry,
    SubagentRequest, SubagentSeed, SubagentStarted, TeamErrorCode, TeamService,
    agent_options_plugin, agent_plugin, approval_plugin, commands_plugin, subagent_jobs_plugin,
    subagent_plugin, team_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_llm::CapabilitySupport;
use heycode_session::{
    Session, SessionEventKind, TeamChange, TeamId, TeamMemberId, TeamMessageId, TeamRole,
    TeamTaskId, TeamTaskState,
};
use tokio_util::sync::CancellationToken;

fn execution_plugin() -> Box<dyn Plugin> {
    heycode_exec::local_execution_plugin(
        heycode_exec::LocalShellConfig::platform(
            std::env::current_dir().unwrap(),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
    )
}

fn world(root: std::path::PathBuf) -> heycode_core::Context {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(root.clone()),
        heycode_prompt::prompt_plugin(),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            heycode_llm::LlmSelection {
                provider_name: "fake".to_owned(),
                model: "m".to_owned(),
            },
            vec![Arc::new(
                heycode_llm::testing::FakeProvider::new(Vec::new()),
            )],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        heycode_agent::compactions_plugin(),
        subagent_plugin(root, 3),
        agent_options_plugin(AgentOptions::default()),
        agent_plugin(),
        subagent_jobs_plugin(),
        team_plugin(),
        heycode_agent::work_plugin_with_teams(),
    ];
    compose(&plugins).unwrap()
}

struct Handle {
    fail_close: bool,
    id: SubagentId,
}

#[async_trait::async_trait]
impl SubagentHandle for Handle {
    fn id(&self) -> &SubagentId {
        &self.id
    }

    fn label(&self) -> &str {
        "worker"
    }

    async fn send(
        &self,
        text: &str,
        _cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        Ok(format!("completed {text}"))
    }

    fn interrupt(&self) -> bool {
        false
    }

    async fn close(&self, _cancellation: CancellationToken) -> Result<(), SubagentError> {
        if self.fail_close {
            return Err(SubagentError::new(
                heycode_agent::SubagentErrorCode::Failed,
                "uncertain fixture close",
            ));
        }
        Ok(())
    }
}

struct Provider {
    descriptor: SubagentProviderDescriptor,
}

#[async_trait::async_trait]
impl SubagentProvider for Provider {
    fn descriptor(&self) -> &SubagentProviderDescriptor {
        &self.descriptor
    }

    async fn start(
        &self,
        _request: SubagentRequest,
        _cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        let id = SubagentId::new("worker-1").unwrap();
        Ok(SubagentStarted {
            id: id.clone(),
            text: "ready".to_owned(),
            handle: Some(Arc::new(Handle {
                id,
                fail_close: _request.label() == "uncertain-close",
            })),
        })
    }
}

async fn fixture() -> (
    tempfile::TempDir,
    Arc<std::sync::Mutex<Session>>,
    Arc<SubagentRegistry>,
    heycode_agent::SubagentAuthority,
    heycode_agent::SubagentAuthority,
    Arc<TeamService>,
) {
    let root = tempfile::tempdir().unwrap();
    let session = Arc::new(std::sync::Mutex::new(Session::create(root.path()).unwrap()));
    let registry = Arc::new(SubagentRegistry::new());
    registry
        .register(Arc::new(Provider {
            descriptor: SubagentProviderDescriptor::new(
                "fixture",
                "Fixture",
                SubagentCapabilities {
                    fork: CapabilitySupport::Unsupported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                },
            )
            .unwrap(),
        }))
        .unwrap();
    let owner = SubagentId::new(session.lock().unwrap().id().as_str()).unwrap();
    let root_authority = registry.root_authority(owner);
    let started = registry
        .start(
            SubagentRequest::with_authority(
                "worker",
                "wait for tasks",
                SubagentSeed::Fresh,
                SubagentContinuation::Continuable,
                root_authority.clone(),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let child_authority = registry
        .authority_for_child(&root_authority, &started.id)
        .unwrap();
    let service = Arc::new(TeamService::new(
        session.clone(),
        registry.clone(),
        root_authority.clone(),
    ));
    (
        root,
        session,
        registry,
        root_authority,
        child_authority,
        service,
    )
}

#[tokio::test]
async fn authority_revision_mailbox_and_wait_are_durable_and_bounded() {
    let (_root, _session, _registry, lead, worker, service) = fixture().await;
    let team = TeamId::new("team").unwrap();
    service.create(&lead, team.clone(), "Lead").unwrap();
    let view = service
        .add_member(
            &lead,
            &team,
            0,
            worker.owner().clone(),
            "Worker",
            TeamRole::Worker,
        )
        .unwrap();
    assert_eq!(view.revision(), 1);
    assert_eq!(
        service
            .send_mail(
                &worker,
                &team,
                0,
                "stale",
                TeamMemberId::new(lead.owner().as_str()).unwrap(),
                "stale",
            )
            .unwrap_err()
            .code(),
        TeamErrorCode::Conflict
    );

    let waiting = {
        let service = service.clone();
        let team = team.clone();
        let waiting_worker = worker.clone();
        tokio::spawn(async move {
            service
                .wait_for_change(
                    &waiting_worker,
                    &team,
                    1,
                    std::time::Duration::from_secs(2),
                    CancellationToken::new(),
                )
                .await
        })
    };
    service
        .send_mail(
            &worker,
            &team,
            1,
            "mail-1",
            TeamMemberId::new(lead.owner().as_str()).unwrap(),
            "worker update",
        )
        .unwrap();
    assert_eq!(waiting.await.unwrap().unwrap().revision(), 2);
    assert_eq!(
        service
            .claim_mail(&lead, &team, 2, &TeamMessageId::new("mail-1").unwrap(),)
            .unwrap(),
        "worker update"
    );

    let foreign_registry = SubagentRegistry::new();
    let foreign = foreign_registry.root_authority(SubagentId::new("foreign").unwrap());
    assert_eq!(
        service
            .send_mail(
                &foreign,
                &team,
                3,
                "mail-2",
                TeamMemberId::new(lead.owner().as_str()).unwrap(),
                "probe",
            )
            .unwrap_err()
            .code(),
        TeamErrorCode::Refused
    );
}

#[tokio::test]
async fn interrupted_task_recovery_blocks_instead_of_inventing_completion() {
    let (_root, session, _registry, lead, worker, service) = fixture().await;
    let team = TeamId::new("team").unwrap();
    service.create(&lead, team.clone(), "Lead").unwrap();
    service
        .add_member(
            &lead,
            &team,
            0,
            worker.owner().clone(),
            "Worker",
            TeamRole::Worker,
        )
        .unwrap();
    service
        .create_task(
            &lead,
            &team,
            1,
            TeamTaskId::new("task").unwrap(),
            "Do work",
            TeamMemberId::new(worker.owner().as_str()).unwrap(),
            Vec::new(),
        )
        .unwrap();
    session
        .lock()
        .unwrap()
        .append(SessionEventKind::TeamChange {
            change: Box::new(
                TeamChange::task_state_changed(
                    &team,
                    3,
                    &TeamMemberId::new(lead.owner().as_str()).unwrap(),
                    &TeamTaskId::new("task").unwrap(),
                    1,
                    TeamTaskState::InProgress,
                    None,
                )
                .unwrap(),
            ),
        })
        .unwrap();
    assert_eq!(service.recover_interrupted(&lead, &team).unwrap(), 1);
    let projection = service.projection().unwrap();
    let task = projection
        .team(&team)
        .unwrap()
        .task(&TeamTaskId::new("task").unwrap())
        .unwrap();
    assert_eq!(task.state(), TeamTaskState::Blocked);
    assert_eq!(
        task.result_summary(),
        Some("interrupted before durable settlement")
    );
}

#[tokio::test]
async fn dispatched_team_task_commits_before_job_and_inbox_publication() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(root.path().to_path_buf());
    let registry = context
        .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    registry
        .register(Arc::new(Provider {
            descriptor: SubagentProviderDescriptor::new(
                "team-fixture",
                "Team fixture",
                SubagentCapabilities {
                    fork: CapabilitySupport::Unsupported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                },
            )
            .unwrap(),
        }))
        .unwrap();
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let started = tools
        .get("task")
        .unwrap()
        .run(
            serde_json::json!({
                "label":"worker",
                "prompt":"wait for team work",
                "background":false,
                "provider":"team-fixture",
                "mode":"continuable"
            }),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    let child_id = registry
        .task_snapshots()
        .into_iter()
        .find(|task| task.provider == "team-fixture")
        .unwrap()
        .id;
    assert!(started.as_str().unwrap().contains(&child_id));
    let team = tools.get("team").unwrap();
    team.run(
        serde_json::json!({"action":"create","team_id":"team","display":"Lead","roles":[]}),
        &heycode_tools::ToolCtx::default(),
    )
    .await
    .unwrap();
    team.run(
        serde_json::json!({
            "action":"add_member","team_id":"team","revision":0,
            "child_id":child_id,"display":"Worker","role":"worker"
        }),
        &heycode_tools::ToolCtx::default(),
    )
    .await
    .unwrap();
    team.run(
        serde_json::json!({
            "action":"create_task","team_id":"team","revision":1,
            "task_id":"task","title":"implement","assignee":child_id,"auto_dispatch":false
        }),
        &heycode_tools::ToolCtx::default(),
    )
    .await
    .unwrap();
    let dispatched = team
        .run(
            serde_json::json!({
                "action":"dispatch_task","team_id":"team","revision":2,"task_id":"task"
            }),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    let job_id = heycode_agent::JobId::parse(dispatched["job_id"].as_str().unwrap()).unwrap();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            jobs.wait_for_settlement(&job_id),
        )
        .await
        .unwrap()
        .unwrap(),
        heycode_agent::JobOutcome::Completed
    );
    let teams = context
        .get::<TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let projection = teams.projection().unwrap();
    assert_eq!(
        projection
            .team(&TeamId::new("team").unwrap())
            .unwrap()
            .task(&TeamTaskId::new("task").unwrap())
            .unwrap()
            .state(),
        TeamTaskState::Blocked
    );
    let work = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let task_id = heycode_session::team_work_id(
        &TeamId::new("team").unwrap(),
        &TeamTaskId::new("task").unwrap(),
    )
    .unwrap();
    let item = work
        .get("task_get")
        .unwrap()
        .run(
            serde_json::json!({"id":task_id}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(item["fields"]["status"], "blocked");
    assert!(
        item["result_summary"]
            .as_str()
            .unwrap()
            .contains("awaiting review")
    );
    let completed = work.get("task_update").unwrap().run(serde_json::json!({"id":task_id,"expected_revision":item["revision"],"status":"completed"}), &heycode_tools::ToolCtx::default()).await.unwrap();
    assert_eq!(completed["fields"]["status"], "completed");
    assert_eq!(completed["result_summary"], item["result_summary"]);
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert_eq!(agent.pending_inbox().next_turn, 1);
    context.shutdown();
}

#[tokio::test]
async fn shutdown_archive_resume_and_reassignment_preserve_work_and_refuse_foreign_control() {
    use heycode_session::TeamLifecycle;
    let (root, session, registry, lead, worker, service) = fixture().await;
    let team = TeamId::new("lifecycle").unwrap();
    service.create(&lead, team.clone(), "Lead").unwrap();
    service
        .add_member(
            &lead,
            &team,
            0,
            worker.owner().clone(),
            "Worker",
            TeamRole::Worker,
        )
        .unwrap();
    let task_id = TeamTaskId::new("work-a").unwrap();
    service
        .create_task(
            &lead,
            &team,
            1,
            task_id.clone(),
            "Original work",
            TeamMemberId::new(worker.owner().as_str()).unwrap(),
            vec![],
        )
        .unwrap();
    assert_eq!(
        service.archive(&lead, &team, 2).unwrap_err().code(),
        TeamErrorCode::Conflict
    );
    assert_eq!(
        service
            .remove_member(
                &lead,
                &team,
                2,
                TeamMemberId::new(worker.owner().as_str()).unwrap()
            )
            .unwrap_err()
            .code(),
        TeamErrorCode::Conflict
    );
    assert_eq!(
        service
            .shutdown_team(&worker, &team, 2, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        TeamErrorCode::Refused
    );
    let stopped = service
        .shutdown_team(&lead, &team, 2, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(stopped.lifecycle(), TeamLifecycle::Stopped);
    assert_eq!(heycode_agent::render_team(&stopped)["lifecycle"], "stopped");
    assert_eq!(
        stopped.task(&task_id).unwrap().state(),
        TeamTaskState::Pending
    );
    assert!(registry.child_for(&lead, worker.owner()).is_none());
    assert_eq!(
        registry.task_snapshots_for(&lead)[0].state,
        heycode_agent::TaskState::Closed
    );
    let archived = service.archive(&lead, &team, stopped.revision()).unwrap();
    assert_eq!(archived.lifecycle(), TeamLifecycle::Archived);
    assert_eq!(
        heycode_agent::render_team(&archived)["lifecycle"],
        "archived"
    );
    assert!(
        service
            .create_task(
                &lead,
                &team,
                archived.revision(),
                TeamTaskId::new("no").unwrap(),
                "No admission",
                TeamMemberId::new(lead.owner().as_str()).unwrap(),
                vec![]
            )
            .is_err()
    );
    let resumed = service.resume(&lead, &team, archived.revision()).unwrap();
    assert_eq!(resumed.lifecycle(), TeamLifecycle::Active);
    assert!(registry.child_for(&lead, worker.owner()).is_none());
    let task = resumed.task(&task_id).unwrap();
    let revised = task
        .revised(
            "Reassigned work".into(),
            TeamMemberId::new(lead.owner().as_str()).unwrap(),
            vec![],
            TeamTaskState::Pending,
        )
        .unwrap();
    let updated = service
        .update_task(&lead, &team, resumed.revision(), task.revision(), revised)
        .unwrap();
    assert_eq!(updated.task(&task_id).unwrap().revision(), 2);
    let removed = service
        .remove_member(
            &lead,
            &team,
            updated.revision(),
            TeamMemberId::new(worker.owner().as_str()).unwrap(),
        )
        .unwrap();
    assert_eq!(removed.members().len(), 1);
    assert_eq!(removed.task(&task_id).unwrap().title(), "Reassigned work");
    let replay = heycode_session::project_teams(session.lock().unwrap().events()).unwrap();
    assert_eq!(replay.team(&team).unwrap(), &removed);
    let directory = root.path().join(session.lock().unwrap().id().as_str());
    drop(service);
    drop(session);
    let reopened = Session::open(directory).unwrap();
    assert_eq!(
        heycode_session::project_teams(reopened.events())
            .unwrap()
            .team(&team)
            .unwrap(),
        &removed
    );
}

struct WaitingTeamProvider {
    descriptor: SubagentProviderDescriptor,
    entered: Arc<std::sync::atomic::AtomicUsize>,
}
struct WaitingTeamHandle {
    id: SubagentId,
    entered: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait::async_trait]
impl SubagentHandle for WaitingTeamHandle {
    fn id(&self) -> &SubagentId {
        &self.id
    }
    fn label(&self) -> &str {
        "Waiting team worker"
    }
    async fn send(
        &self,
        _: &str,
        cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        cancellation.cancelled().await;
        Err(SubagentError::new(
            heycode_agent::SubagentErrorCode::Cancelled,
            "cancelled",
        ))
    }
    fn interrupt(&self) -> bool {
        false
    }
    async fn close(&self, _: CancellationToken) -> Result<(), SubagentError> {
        Ok(())
    }
}
#[async_trait::async_trait]
impl SubagentProvider for WaitingTeamProvider {
    fn descriptor(&self) -> &SubagentProviderDescriptor {
        &self.descriptor
    }
    async fn start(
        &self,
        request: SubagentRequest,
        _: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        let id = SubagentId::new(request.label()).unwrap();
        Ok(SubagentStarted {
            id: id.clone(),
            text: "Ready".into(),
            handle: Some(Arc::new(WaitingTeamHandle {
                id,
                entered: self.entered.clone(),
            })),
        })
    }
}

#[tokio::test]
async fn shutdown_settles_active_work_without_touching_another_teams_worker() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(root.path().to_path_buf());
    let registry = context
        .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let lead =
        registry.root_authority(SubagentId::new(session.lock().unwrap().id().as_str()).unwrap());
    let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry
        .register(Arc::new(WaitingTeamProvider {
            descriptor: SubagentProviderDescriptor::new(
                "waiting-team",
                "Waiting team",
                SubagentCapabilities {
                    fork: CapabilitySupport::Unsupported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                },
            )
            .unwrap(),
            entered: entered.clone(),
        }))
        .unwrap();
    let service = context
        .get::<TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let mut children = std::collections::BTreeMap::new();
    for name in ["a", "b"] {
        let started = registry
            .start(
                SubagentRequest::with_authority(
                    name,
                    "Wait",
                    SubagentSeed::Fresh,
                    SubagentContinuation::Continuable,
                    lead.clone(),
                )
                .unwrap()
                .with_provider(heycode_agent::SubagentProviderId::new("waiting-team").unwrap()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let team = TeamId::new(name).unwrap();
        service.create(&lead, team.clone(), "Lead").unwrap();
        children.insert(name, started.id.clone());
        service
            .add_member(&lead, &team, 0, started.id, name, TeamRole::Worker)
            .unwrap();
    }
    let a = TeamId::new("a").unwrap();
    let b = TeamId::new("b").unwrap();
    assert_eq!(
        service
            .add_member(
                &lead,
                &b,
                1,
                children["a"].clone(),
                "Shared",
                TeamRole::Worker
            )
            .unwrap_err()
            .code(),
        TeamErrorCode::Conflict
    );
    let task = TeamTaskId::new("running").unwrap();
    service
        .create_task(
            &lead,
            &a,
            1,
            task.clone(),
            "Wait for cancellation",
            TeamMemberId::new(children["a"].as_str()).unwrap(),
            vec![],
        )
        .unwrap();
    let job = service.dispatch_task(&lead, &a, 2, &task).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while entered.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let stopped = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        service.shutdown_team(&lead, &a, 3, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stopped.task(&task).unwrap().state(), TeamTaskState::Blocked);
    assert!(registry.child_for(&lead, &children["a"].clone()).is_none());
    assert!(registry.child_for(&lead, &children["b"].clone()).is_some());
    assert_eq!(service.view(&lead, &b).unwrap().revision(), 1);
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            jobs.wait_for_task_exit(&job)
        )
        .await
        .unwrap()
        .unwrap(),
        heycode_agent::JobOutcome::Cancelled
    );
    context.shutdown();
}

#[tokio::test]
async fn unified_work_tools_keep_one_team_owner_with_idempotency_dependencies_and_tombstones() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(root.path().to_path_buf());
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let cx = heycode_tools::ToolCtx::default();
    tools
        .get("team")
        .unwrap()
        .run(
            serde_json::json!({"action":"create","team_id":"work-team","roles":[]}),
            &cx,
        )
        .await
        .unwrap();
    let create = tools.get("task_create").unwrap();
    let update = tools.get("task_update").unwrap();
    let get = tools.get("task_get").unwrap();
    let list = tools.get("task_list").unwrap();
    let request = serde_json::json!({"team_id":"work-team","request_key":"a","subject":"First","description":"Acceptance stays separate from results","metadata":{"source":"test"}});
    let a = create.run(request.clone(), &cx).await.unwrap();
    let id_a = a["id"].as_str().unwrap();
    assert!(a["fields"]["owner"].is_null());
    let team_projection = heycode_session::project_teams(session.lock().unwrap().events()).unwrap();
    let team = team_projection
        .team(&TeamId::new("work-team").unwrap())
        .unwrap();
    assert!(
        !team
            .task(&TeamTaskId::new("a").unwrap())
            .unwrap()
            .assigned()
    );
    assert!(
        !team
            .task(&TeamTaskId::new("a").unwrap())
            .unwrap()
            .is_runnable(team)
    );
    let b = create.run(serde_json::json!({"team_id":"work-team","request_key":"b","subject":"Second","dependencies":[id_a]}), &cx).await.unwrap();
    let id_b = b["id"].as_str().unwrap();
    assert!(
        update
            .run(
                serde_json::json!({"id":id_b,"expected_revision":1,"status":"in_progress"}),
                &cx
            )
            .await
            .is_err()
    );
    assert!(
        update
            .run(
                serde_json::json!({"id":id_a,"expected_revision":1,"dependencies":[id_b]}),
                &cx
            )
            .await
            .is_err()
    );
    let done = update
        .run(
            serde_json::json!({"id":id_a,"expected_revision":1,"status":"completed"}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(done["revision"], 2);
    let count = session.lock().unwrap().events().len();
    assert_eq!(create.run(request, &cx).await.unwrap()["revision"], 2);
    assert_eq!(session.lock().unwrap().events().len(), count);
    assert!(create.run(serde_json::json!({"team_id":"work-team","request_key":"a","subject":"Conflicting payload"}), &cx).await.is_err());
    update
        .run(
            serde_json::json!({"id":id_b,"expected_revision":1,"status":"in_progress"}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(
        get.run(serde_json::json!({"id":id_a}), &cx).await.unwrap()["fields"]["description"],
        "Acceptance stays separate from results"
    );
    let first = list.run(serde_json::json!({"limit":1}), &cx).await.unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let second = list
        .run(
            serde_json::json!({"limit":1,"after":first["next_cursor"]}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert!(second["next_cursor"].is_null());
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    assert!(
        update
            .run(
                serde_json::json!({"id":id_a,"expected_revision":2,"status":"deleted"}),
                &cx
            )
            .await
            .is_err()
    );
    update
        .run(
            serde_json::json!({"id":id_b,"expected_revision":2,"status":"deleted"}),
            &cx,
        )
        .await
        .unwrap();
    update
        .run(
            serde_json::json!({"id":id_a,"expected_revision":2,"status":"deleted"}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(
        list.run(serde_json::json!({}), &cx).await.unwrap()["items"],
        serde_json::json!([])
    );
    assert_eq!(
        list.run(serde_json::json!({"include_deleted":true}), &cx)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let locked = session.lock().unwrap();
    assert!(
        locked
            .events()
            .iter()
            .all(|event| !matches!(event.kind, SessionEventKind::WorkChange { .. }))
    );
    let directory = root.path().join(locked.id().as_str());
    let expected = heycode_session::project_work_items(locked.events()).unwrap();
    drop(locked);
    context.shutdown();
    drop(context);
    drop(session);
    drop(tools);
    drop(create);
    drop(update);
    drop(get);
    drop(list);
    let reopened = Session::open(directory).unwrap();
    assert_eq!(
        heycode_session::project_work_items(reopened.events()).unwrap(),
        expected
    );
}

#[tokio::test]
async fn uncertain_member_close_keeps_shutdown_and_archive_blocked() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(root.path().to_path_buf());
    let registry = context
        .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    registry
        .register(Arc::new(Provider {
            descriptor: SubagentProviderDescriptor::new(
                "team-fixture",
                "Team fixture",
                SubagentCapabilities {
                    fork: CapabilitySupport::Unsupported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                },
            )
            .unwrap(),
        }))
        .unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let lead =
        registry.root_authority(SubagentId::new(session.lock().unwrap().id().as_str()).unwrap());
    let started = registry
        .start(
            SubagentRequest::with_authority(
                "uncertain-close",
                "Ready",
                SubagentSeed::Fresh,
                SubagentContinuation::Continuable,
                lead.clone(),
            )
            .unwrap()
            .with_provider(heycode_agent::SubagentProviderId::new("team-fixture").unwrap()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let service = context
        .get::<TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let team = TeamId::new("uncertain").unwrap();
    service.create(&lead, team.clone(), "Lead").unwrap();
    service
        .add_member(
            &lead,
            &team,
            0,
            started.id.clone(),
            "Worker",
            TeamRole::Worker,
        )
        .unwrap();
    assert_eq!(
        service
            .shutdown_team(&lead, &team, 1, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        TeamErrorCode::Failed
    );
    let view = service.view(&lead, &team).unwrap();
    assert_eq!(
        view.lifecycle(),
        heycode_session::TeamLifecycle::ShuttingDown
    );
    assert!(registry.child_for(&lead, &started.id).is_none());
    assert!(
        registry
            .children_for(&lead)
            .iter()
            .any(|(id, _)| id == &started.id)
    );
    assert_eq!(
        service
            .shutdown_team(&lead, &team, view.revision(), CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        TeamErrorCode::Failed
    );
    assert_eq!(
        service
            .archive(&lead, &team, view.revision())
            .unwrap_err()
            .code(),
        TeamErrorCode::Conflict
    );
    assert_eq!(
        service
            .resume(&lead, &team, view.revision())
            .unwrap_err()
            .code(),
        TeamErrorCode::Conflict
    );
    context.shutdown();
}

struct MailRecordingProvider {
    requests: Arc<std::sync::Mutex<Vec<heycode_llm::ChatRequest>>>,
}
impl heycode_llm::Provider for MailRecordingProvider {
    fn info(&self) -> heycode_llm::ProviderInfo {
        heycode_llm::ProviderInfo {
            name: "mail-fixture".into(),
            default_model: "m".into(),
        }
    }
    fn stream(&self, request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
        let mut requests = self.requests.lock().unwrap();
        let work_request = |request: &heycode_llm::ChatRequest| {
            request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == heycode_llm::Role::User)
                .is_some_and(|message| message.content.contains("FAIL-TEAM-ONCE"))
        };
        let fail = work_request(&request) && !requests.iter().any(work_request);
        requests.push(request);
        drop(requests);
        if fail {
            return Box::pin(futures::stream::iter(vec![Err(
                heycode_llm::LlmError::InvalidResponse("intentional team worker failure".into()),
            )]));
        }
        Box::pin(futures::stream::iter(vec![
            Ok(heycode_llm::StreamChunk::TextDelta(
                "Mail acknowledged".into(),
            )),
            Ok(heycode_llm::StreamChunk::Finish(
                heycode_llm::FinishReason::Stop,
            )),
        ]))
    }
}

#[tokio::test]
async fn native_team_mail_is_delivered_once_and_claim_survives_owner_reconstruction() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(root.path().to_path_buf());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let registry = context
        .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let service = context
        .get::<TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(MailRecordingProvider {
            requests: requests.clone(),
        }))
        .unwrap();
    agent.set_inference_route("mail-fixture", "m", None);
    let authority = registry
        .root_authority(SubagentId::new(agent.session().lock().unwrap().id().as_str()).unwrap());
    let started = registry
        .start(
            SubagentRequest::with_authority(
                "Recipient",
                "Wait for team mail",
                SubagentSeed::Fresh,
                SubagentContinuation::Continuable,
                authority.clone(),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let child = registry.native_child_for(&authority, &started.id).unwrap();
    let worker = registry
        .authority_for_child(&authority, &started.id)
        .unwrap();
    let team = TeamId::new("native-mail").unwrap();
    service.create(&authority, team.clone(), "Lead").unwrap();
    service
        .add_member(
            &authority,
            &team,
            0,
            started.id.clone(),
            "Recipient",
            TeamRole::Worker,
        )
        .unwrap();
    service
        .send_mail(
            &authority,
            &team,
            1,
            "mail-once",
            TeamMemberId::new(started.id.as_str()).unwrap(),
            "MAIL-ONCE-SENTINEL",
        )
        .unwrap();
    assert_eq!(service.deliver_pending(&authority, &team).unwrap(), 1);
    assert_eq!(service.deliver_pending(&authority, &team).unwrap(), 0);
    for job in jobs.list() {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            jobs.wait_for_task_exit(&job.id),
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "bootstrap and one mail turn only"
    );
    assert_eq!(child.session().lock().unwrap().events().iter().filter(|event| matches!(&event.kind, SessionEventKind::UserMessage { text } if text.contains("MAIL-ONCE-SENTINEL"))).count(), 1);
    let revision = service.view(&authority, &team).unwrap().revision();
    assert_eq!(
        service
            .claim_mail(
                &worker,
                &team,
                revision,
                &TeamMessageId::new("mail-once").unwrap()
            )
            .unwrap(),
        "MAIL-ONCE-SENTINEL"
    );
    let reconstructed = Arc::new(TeamService::new(
        agent.session().clone(),
        registry.clone(),
        authority.clone(),
    ));
    reconstructed
        .attach_job_host(&agent, (*jobs).clone())
        .unwrap();
    assert_eq!(reconstructed.deliver_pending(&authority, &team).unwrap(), 0);
    let restored = reconstructed.view(&authority, &team).unwrap();
    assert_eq!(restored.mail().len(), 1);
    assert!(restored.mail()[0].1 && restored.mail()[0].2);
    assert_eq!(requests.lock().unwrap().len(), 2);
    registry
        .close_child_for(&authority, &started.id, CancellationToken::new())
        .await
        .unwrap();
    reconstructed.dispose();
    context.shutdown();
}

#[tokio::test]
async fn native_team_failure_replacement_and_dependent_review_keep_durable_outcomes_distinct() {
    let root = tempfile::tempdir().unwrap();
    let mut context = world(root.path().to_path_buf());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let registry = context
        .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let service = context
        .get::<TeamService>(heycode_agent::SERVICE_TEAMS)
        .unwrap();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let tools = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .register(Arc::new(MailRecordingProvider {
            requests: requests.clone(),
        }))
        .unwrap();
    agent.set_inference_route("mail-fixture", "m", None);
    let authority = registry
        .root_authority(SubagentId::new(agent.session().lock().unwrap().id().as_str()).unwrap());
    let team = TeamId::new("dependent-native").unwrap();
    service.create(&authority, team.clone(), "Lead").unwrap();
    let mut members = Vec::new();
    for label in ["Initial worker", "Reviewer", "Replacement"] {
        let child = registry
            .start(
                SubagentRequest::with_authority(
                    label,
                    "Await assigned team work",
                    SubagentSeed::Fresh,
                    SubagentContinuation::Continuable,
                    authority.clone(),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let view = service.view(&authority, &team).unwrap();
        service
            .add_member(
                &authority,
                &team,
                view.revision(),
                child.id.clone(),
                label,
                if label == "Reviewer" {
                    TeamRole::Reviewer
                } else {
                    TeamRole::Worker
                },
            )
            .unwrap();
        members.push(child.id);
    }
    let work = TeamTaskId::new("implementation").unwrap();
    let review = TeamTaskId::new("review").unwrap();
    let view = service.view(&authority, &team).unwrap();
    service
        .create_task(
            &authority,
            &team,
            view.revision(),
            work.clone(),
            "FAIL-TEAM-ONCE implementation",
            TeamMemberId::new(members[0].as_str()).unwrap(),
            vec![],
        )
        .unwrap();
    let view = service.view(&authority, &team).unwrap();
    service
        .create_task(
            &authority,
            &team,
            view.revision(),
            review.clone(),
            "Review implementation",
            TeamMemberId::new(members[1].as_str()).unwrap(),
            vec![work.clone()],
        )
        .unwrap();
    let view = service.view(&authority, &team).unwrap();
    assert!(
        service
            .dispatch_task(&authority, &team, view.revision(), &review)
            .is_err()
    );
    let job = service
        .dispatch_task(&authority, &team, view.revision(), &work)
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            jobs.wait_for_task_exit(&job)
        )
        .await
        .unwrap()
        .unwrap(),
        heycode_agent::JobOutcome::Failed
    );
    let view = service.view(&authority, &team).unwrap();
    let failed = view.task(&work).unwrap();
    assert_eq!(failed.state(), TeamTaskState::Failed);
    let reassigned = failed
        .revised(
            failed.title().into(),
            TeamMemberId::new(members[2].as_str()).unwrap(),
            vec![],
            TeamTaskState::Pending,
        )
        .unwrap();
    service
        .update_task(
            &authority,
            &team,
            view.revision(),
            failed.revision(),
            reassigned,
        )
        .unwrap();
    let view = service.view(&authority, &team).unwrap();
    service
        .remove_member(
            &authority,
            &team,
            view.revision(),
            TeamMemberId::new(members[0].as_str()).unwrap(),
        )
        .unwrap();
    registry
        .close_child_for(&authority, &members[0], CancellationToken::new())
        .await
        .unwrap();
    for task in [&work, &review] {
        let view = service.view(&authority, &team).unwrap();
        let job = service
            .dispatch_task(&authority, &team, view.revision(), task)
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                jobs.wait_for_task_exit(&job)
            )
            .await
            .unwrap()
            .unwrap(),
            heycode_agent::JobOutcome::Completed
        );
        let view = service.view(&authority, &team).unwrap();
        let finished = view.task(task).unwrap();
        assert_eq!(
            finished.state(),
            TeamTaskState::Blocked,
            "worker completion requires review"
        );
        let item = heycode_session::WorkItem::from_team_task(&team, finished).unwrap();
        tools.get("task_update").unwrap().run(serde_json::json!({"id":item.id(),"expected_revision":item.revision(),"status":"completed"}), &heycode_tools::ToolCtx::default()).await.unwrap();
    }
    let view = service.view(&authority, &team).unwrap();
    assert!(
        view.tasks()
            .iter()
            .all(|task| task.state() == TeamTaskState::Completed)
    );
    let stopped = service
        .shutdown_team(&authority, &team, view.revision(), CancellationToken::new())
        .await
        .unwrap();
    let archived = service
        .archive(&authority, &team, stopped.revision())
        .unwrap();
    assert_eq!(
        heycode_agent::render_team(&archived)["lifecycle"],
        "archived"
    );
    let parent_path = agent
        .session()
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    let expected = service.projection().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        6,
        "three member starts, one failed work attempt, replacement attempt and review"
    );
    assert!(requests.last().unwrap().messages.iter().any(|message| {
        message.content.contains("Completed dependency results")
            && message.content.contains("Mail acknowledged")
    }));
    drop(requests);
    context.shutdown();
    drop(context);
    drop(tools);
    drop(jobs);
    drop(service);
    drop(registry);
    drop(agent);
    let reopened = Session::open(parent_path).unwrap();
    assert_eq!(
        heycode_session::project_teams(reopened.events()).unwrap(),
        expected
    );
}

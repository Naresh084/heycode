//! O10 revisioned goal state, rearm, round cap, wake cap, and resume.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_agent::{
    AgentOptions, GoalActivation, GoalErrorCode, GoalPolicy, GoalService, SERVICE_GOALS,
    agent_options_plugin, agent_plugin, approval_plugin, commands_plugin, compactions_plugin,
    goal_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_exec::{LocalShellConfig, local_execution_plugin};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    FinishReason, LlmSelection, Provider, StreamChunk, llm_plugin, model_catalog_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_session::{GoalRef, InboxSource, Session, session_plugin, session_resume_plugin};
use heycode_tools::tools_plugin;

struct World {
    context: heycode_core::Context,
    agent: Arc<heycode_agent::Agent>,
    session: Arc<std::sync::Mutex<Session>>,
    goals: Arc<GoalService>,
}

fn world(session: Box<dyn Plugin>, root: &std::path::Path) -> World {
    world_with_policy(session, root, GoalPolicy::new(3, 1).unwrap())
}

fn world_with_policy(
    session: Box<dyn Plugin>,
    root: &std::path::Path,
    policy: GoalPolicy,
) -> World {
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![
        vec![
            StreamChunk::TextDelta("round one".to_owned()),
            StreamChunk::Finish(FinishReason::Stop),
        ],
        vec![
            StreamChunk::TextDelta("round two".to_owned()),
            StreamChunk::Finish(FinishReason::Stop),
        ],
    ]));
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
        goal_plugin(policy),
    ];
    let context = compose(&plugins).unwrap();
    World {
        agent: context
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .unwrap(),
        session: context
            .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
            .unwrap(),
        goals: context.get::<GoalService>(SERVICE_GOALS).unwrap(),
        context,
    }
}

#[tokio::test]
async fn cas_rearm_round_and_wake_budgets_survive_resume_without_auto_activation() {
    let root = tempfile::tempdir().unwrap();
    let first = world(session_plugin(root.path().to_path_buf()), root.path());
    let created = first.goals.create("finish the lane", None).unwrap();
    assert_eq!(created.activation(), GoalActivation::Armed);
    assert_eq!(created.snapshot().revision(), 1);
    {
        let session = first
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pending = &session.inbox().next_turn()[0];
        assert!(matches!(
            pending.source(),
            InboxSource::Goal {
                revision: 1,
                round: 1,
                ..
            }
        ));
    }

    let stale = GoalRef::new(created.snapshot().id().clone(), 99).unwrap();
    assert_eq!(
        first.goals.pause(&stale).unwrap_err().code(),
        GoalErrorCode::StaleRevision
    );

    first
        .agent
        .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let after_round = first.goals.get().unwrap().unwrap();
    assert_eq!(after_round.rounds_started(), 1);
    assert_eq!(after_round.activation(), GoalActivation::Disarmed);
    assert!(first.agent.pending_inbox().is_empty());

    let rearmed = first
        .goals
        .resume(&after_round.snapshot().reference())
        .unwrap();
    assert_eq!(rearmed.snapshot().revision(), 2);
    assert_eq!(rearmed.activation(), GoalActivation::Armed);
    first
        .agent
        .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first.goals.get().unwrap().unwrap().rounds_started(), 2);

    let session_path = first
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    let durable_ref = first.goals.get().unwrap().unwrap().snapshot().reference();
    drop(first.context);

    let resumed = world(session_resume_plugin(session_path), root.path());
    let view = resumed.goals.get().unwrap().unwrap();
    assert_eq!(view.snapshot().reference(), durable_ref);
    assert_eq!(view.rounds_started(), 2);
    assert_eq!(view.activation(), GoalActivation::Disarmed);
    assert!(resumed.agent.pending_inbox().is_empty());
}

#[tokio::test]
async fn goal_command_optional_resume_message_is_logged_by_the_domain_once() {
    let root = tempfile::tempdir().unwrap();
    let world = world(session_plugin(root.path().to_path_buf()), root.path());
    let commands = world
        .context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let goal = commands.get("goal").unwrap().unwrap();
    goal.execute(&world.agent, "create finish command coverage")
        .await
        .unwrap();
    goal.execute(&world.agent, "pause").await.unwrap();
    goal.execute(&world.agent, "resume inspect the command boundary")
        .await
        .unwrap();

    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                heycode_session::SessionEventKind::UserMessage { text }
                    if text == "inspect the command boundary"
            ))
            .count(),
        1
    );
    assert!(session.events().iter().all(|event| !matches!(
        &event.kind,
        heycode_session::SessionEventKind::UserMessage { text }
            if text.starts_with("create ") || text == "pause" || text.starts_with("resume ")
    )));
}

#[tokio::test]
async fn default_goal_continues_without_round_or_wake_limits() {
    let root = tempfile::tempdir().unwrap();
    let mut world = world_with_policy(
        session_plugin(root.path().to_path_buf()),
        root.path(),
        GoalPolicy::default(),
    );
    let created = world
        .goals
        .create("finish the requested task", None)
        .unwrap();
    assert_eq!(created.snapshot().max_rounds(), 0);
    for expected in 1..=2 {
        world
            .agent
            .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();
        let current = world.goals.get().unwrap().unwrap();
        assert_eq!(current.rounds_started(), expected);
        assert_eq!(current.activation(), GoalActivation::Armed);
        assert_eq!(world.agent.pending_inbox().next_turn, 1);
    }
    let current = world.goals.get().unwrap().unwrap();
    world.goals.pause(&current.snapshot().reference()).unwrap();
    assert!(world.agent.pending_inbox().is_empty());
    world.context.shutdown();
}

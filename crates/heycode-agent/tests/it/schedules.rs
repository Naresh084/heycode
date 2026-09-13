//! O13 durable timers, enqueue-before-dispatch, restart, and disposal.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_agent::{
    AgentOptions, DurableScheduleService, SERVICE_SCHEDULES, agent_options_plugin, agent_plugin,
    approval_plugin, commands_plugin, compactions_plugin, durable_schedules_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_exec::{LocalShellConfig, local_execution_plugin};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, Provider, llm_plugin, model_catalog_plugin};
use heycode_prompt::prompt_plugin;
use heycode_session::{
    InboxDelivery, InboxMessage, InboxMessageId, InboxSource, InboxTarget, ScheduleChange, Session,
    SessionEventKind, project_schedules, session_plugin, session_resume_plugin,
};
use heycode_tools::tools_plugin;

struct World {
    context: heycode_core::Context,
    session: Arc<std::sync::Mutex<Session>>,
    schedules: Arc<DurableScheduleService>,
}

fn world(session: Box<dyn Plugin>, root: &std::path::Path) -> World {
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
        durable_schedules_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    World {
        session: context
            .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
            .unwrap(),
        schedules: context
            .get::<DurableScheduleService>(SERVICE_SCHEDULES)
            .unwrap(),
        context,
    }
}

async fn wait_for_dispatch(world: &World) {
    for _ in 0..400 {
        let dispatched = {
            let session = world
                .session
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            session.events().iter().any(|event| matches!(
                event.kind,
                SessionEventKind::ScheduleChange { ref change }
                    if matches!(change.as_ref(), heycode_session::ScheduleChange::Dispatch { .. })
            ))
        };
        if dispatched {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("durable schedule did not dispatch")
}

#[tokio::test]
async fn due_timer_flushes_then_enqueues_then_records_dispatch_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let world = world(session_plugin(root.path().to_path_buf()), root.path());
    assert!(
        world
            .context
            .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
            .unwrap()
            .names()
            .iter()
            .any(|name| name == "schedule_create")
    );
    let record = world
        .schedules
        .create_after("recheck the focused gate", Duration::from_millis(20))
        .unwrap();
    wait_for_dispatch(&world).await;

    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let create = session
        .events()
        .iter()
        .position(|event| matches!(
            &event.kind,
            SessionEventKind::ScheduleChange { change }
                if matches!(change.as_ref(), heycode_session::ScheduleChange::Create { schedule, .. } if schedule.id() == record.id())
        ))
        .unwrap();
    let enqueue = session
        .events()
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::AgentInboxSplice { .. }))
        .unwrap();
    let dispatch = session
        .events()
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                SessionEventKind::ScheduleChange { ref change }
                    if matches!(change.as_ref(), heycode_session::ScheduleChange::Dispatch { .. })
            )
        })
        .unwrap();
    assert!(create < enqueue && enqueue < dispatch);
    assert!(matches!(
        session.inbox().next_turn()[0].source(),
        InboxSource::Schedule { schedule_id, .. } if schedule_id == record.id()
    ));
    assert!(
        project_schedules(session.events(), session.first_local_seq())
            .unwrap()
            .get(record.id())
            .is_none()
    );
}

#[tokio::test]
async fn active_timer_rearms_from_jsonl_after_restart_but_not_before() {
    let root = tempfile::tempdir().unwrap();
    let first = world(session_plugin(root.path().to_path_buf()), root.path());
    let record = first
        .schedules
        .create_after("resume reminder", Duration::from_millis(200))
        .unwrap();
    let session_path = first
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    drop(first.context);

    let resumed = world(session_resume_plugin(session_path), root.path());
    assert!(
        resumed
            .schedules
            .list()
            .unwrap()
            .iter()
            .any(|row| row.id() == record.id())
    );
    wait_for_dispatch(&resumed).await;
    let session = resumed
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(
        project_schedules(session.events(), session.first_local_seq())
            .unwrap()
            .get(record.id())
            .is_none()
    );
}

#[tokio::test]
async fn restart_finalizes_enqueue_without_dispatch_once_and_never_duplicates_input() {
    let root = tempfile::tempdir().unwrap();
    let mut seeded = Session::create(root.path()).unwrap();
    let schedule_id = heycode_session::ScheduleId::new("recovery-schedule").unwrap();
    let occurrence = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap();
    seeded
        .append(SessionEventKind::ScheduleChange {
            change: Box::new(heycode_session::ScheduleChange::create(
                heycode_session::ScheduleRecord::at(
                    schedule_id.clone(),
                    "recover once",
                    occurrence,
                )
                .unwrap(),
            )),
        })
        .unwrap();
    seeded
        .append(SessionEventKind::AgentInboxSplice {
            target: heycode_session::InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                heycode_session::InboxMessage::with_source(
                    heycode_session::InboxMessageId::new("recovery-schedule-message").unwrap(),
                    heycode_session::InboxDelivery::FollowUp,
                    "scheduled reminder",
                    InboxSource::Schedule {
                        schedule_id,
                        occurrence_at_ms: occurrence,
                    },
                )
                .unwrap(),
            ],
            outcome: None,
        })
        .unwrap();
    seeded.flush().unwrap();
    let session_path = seeded.path().parent().unwrap().to_path_buf();
    drop(seeded);

    let resumed = world(session_resume_plugin(session_path), root.path());
    let session = resumed
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(session.inbox().next_turn().len(), 1);
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                SessionEventKind::ScheduleChange { change }
                    if matches!(change.as_ref(), heycode_session::ScheduleChange::Dispatch { .. })
            ))
            .count(),
        1
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap()
}

#[tokio::test]
async fn composed_tools_create_list_and_delete_real_local_cron() {
    let root = tempfile::tempdir().unwrap();
    let world = world(session_plugin(root.path().to_path_buf()), root.path());
    let tools = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let create = tools.get("schedule_create").unwrap();
    let list = tools.get("schedule_list").unwrap();
    let delete = tools.get("schedule_delete").unwrap();
    assert!(tools.get("schedule_wakeup").is_some());

    let created = create
        .run(
            serde_json::json!({
                "prompt":"check the local build",
                "cron":"7 * * * *",
                "recurring":true
            }),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(created["kind"], "cron");
    assert_eq!(created["cron"], "7 * * * *");
    assert_eq!(created["timezone"], "local");
    assert_eq!(created["state"], "scheduled");
    assert_eq!(created["owner"], "session");
    assert_eq!(created["delivery_mode"], "session_local");
    assert_eq!(created["restored_on_resume"], true);
    assert!(created["expires_at_ms"].as_i64().is_some());
    assert!((0..=30 * 60 * 1_000).contains(&created["jitter_ms"].as_i64().unwrap()));

    let rows = list
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["schedule_id"], created["schedule_id"]);
    assert_eq!(rows[0]["timezone"], "local");

    let deleted = delete
        .run(
            serde_json::json!({"schedule_id":created["schedule_id"]}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    assert!(world.schedules.list().unwrap().is_empty());
}

#[tokio::test]
async fn session_refuses_a_fifty_first_live_schedule() {
    let root = tempfile::tempdir().unwrap();
    let world = world(session_plugin(root.path().to_path_buf()), root.path());
    let target = now_ms() + 60 * 60 * 1_000;
    for index in 0..50 {
        world
            .schedules
            .create_at(format!("bounded {index}"), target + index)
            .unwrap();
    }
    assert_eq!(world.schedules.list().unwrap().len(), 50);
    assert_eq!(
        world
            .schedules
            .create_at("overflow", target + 100)
            .unwrap_err(),
        heycode_agent::DurableScheduleError::Limit
    );
}

#[tokio::test]
async fn delete_settles_a_recurring_dispatch_already_waiting_in_the_inbox() {
    let root = tempfile::tempdir().unwrap();
    let world = world(session_plugin(root.path().to_path_buf()), root.path());
    let record = world
        .schedules
        .create_every("keep checking", Duration::from_secs(1))
        .unwrap();
    wait_for_dispatch(&world).await;
    {
        let session = world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(session.inbox().next_turn().iter().any(|message| {
            matches!(
                message.source(),
                InboxSource::Schedule { schedule_id, .. } if schedule_id == record.id()
            )
        }));
    }

    world.schedules.delete(record.id()).unwrap();
    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(session.inbox().next_turn().is_empty());
    assert!(session.inbox().canceled().iter().any(|settlement| {
        matches!(
            settlement.message().source(),
            InboxSource::Schedule { schedule_id, .. } if schedule_id == record.id()
        )
    }));
    assert!(
        project_schedules(session.events(), session.first_local_seq())
            .unwrap()
            .get(record.id())
            .is_none()
    );
}

#[tokio::test]
async fn wakeup_reschedules_or_stops_and_stop_cancels_an_admitted_occurrence() {
    let root = tempfile::tempdir().unwrap();
    let world = world(session_plugin(root.path().to_path_buf()), root.path());
    let tools = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let created = tools
        .get("schedule_create")
        .unwrap()
        .run(
            serde_json::json!({"prompt":"self paced check","wakeup_seconds":60}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(created["kind"], "wakeup");
    assert_eq!(created["restored_on_resume"], false);
    let first_id =
        heycode_session::ScheduleId::new(created["schedule_id"].as_str().unwrap()).unwrap();
    let first = world
        .schedules
        .list()
        .unwrap()
        .into_iter()
        .find(|record| record.id() == &first_id)
        .unwrap();
    assert_eq!(first.id().as_str().len(), 8);
    assert!(!first.recurring());
    assert!(first.is_wakeup());
    assert_eq!(
        first.expires_at_ms(),
        Some(first.created_at_ms() + 7 * 24 * 60 * 60 * 1_000)
    );

    let second = world
        .schedules
        .reschedule_wakeup(first.id(), Duration::from_secs(120))
        .unwrap();
    assert!(second.scheduled_at_ms() > first.scheduled_at_ms());
    assert!(
        world
            .schedules
            .list_with_state()
            .unwrap()
            .iter()
            .any(|(record, armed)| record.id() == first.id() && *armed)
    );

    let message_id = InboxMessageId::new("admitted-wakeup").unwrap();
    {
        let mut session = world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let next_turn_start = u32::try_from(session.inbox().next_turn().len()).unwrap();
        session
            .append(SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: next_turn_start,
                removed_count: None,
                inserted: vec![
                    InboxMessage::with_source(
                        message_id.clone(),
                        InboxDelivery::FollowUp,
                        "admitted wakeup",
                        InboxSource::Schedule {
                            schedule_id: first.id().clone(),
                            occurrence_at_ms: second.scheduled_at_ms(),
                        },
                    )
                    .unwrap(),
                ],
                outcome: None,
            })
            .unwrap();
        session
            .append(SessionEventKind::ScheduleChange {
                change: Box::new(
                    ScheduleChange::dispatch(
                        first.id().clone(),
                        second.scheduled_at_ms(),
                        message_id,
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session.flush().unwrap();
    }
    assert!(
        world
            .schedules
            .list_with_state()
            .unwrap()
            .iter()
            .any(|(record, armed)| record.id() == first.id() && !*armed)
    );

    let wakeup_tool = tools.get("schedule_wakeup").unwrap();
    let stopped = wakeup_tool
        .run(
            serde_json::json!({"schedule_id":first.id().as_str(),"stop":true}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(stopped["stopped"], true);
    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(session.inbox().next_turn().is_empty());
    assert!(session.inbox().canceled().iter().any(|settlement| {
        matches!(
            settlement.message().source(),
            InboxSource::Schedule { schedule_id, .. } if schedule_id == first.id()
        )
    }));
}

#[tokio::test]
async fn self_paced_wakeup_is_not_restored_after_resume() {
    let root = tempfile::tempdir().unwrap();
    let first = world(session_plugin(root.path().to_path_buf()), root.path());
    let wakeup = first
        .schedules
        .create_wakeup("do not restore", Duration::from_secs(60))
        .unwrap();
    let session_path = first
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    drop(first.context);

    let resumed = world(session_resume_plugin(session_path), root.path());
    assert!(resumed.schedules.list().unwrap().is_empty());
    let session = resumed
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(session.events().iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::ScheduleChange { change }
            if matches!(change.as_ref(), ScheduleChange::Delete { id, .. } if id == wakeup.id())
    )));
}

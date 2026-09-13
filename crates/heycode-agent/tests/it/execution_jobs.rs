//! E09 shell/terminal producers over the effect-owned job registry.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_agent::{
    AgentOptions, CommandRegistry, ExecutionJobService, JobOutcome, JobState,
    SERVICE_EXECUTION_JOBS, agent_options_plugin, agent_plugin, approval_plugin, commands_plugin,
    compactions_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_exec::{
    LocalShellConfig, ShellRequest, local_execution_plugin, terminal_registry_plugin,
};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, Provider, llm_plugin, model_catalog_plugin};
use heycode_prompt::prompt_plugin;
use heycode_session::{InboxDelivery, Session, session_plugin};
use heycode_tools::tools_plugin;

struct World {
    context: heycode_core::Context,
    session: Arc<std::sync::Mutex<Session>>,
    execution: Arc<ExecutionJobService>,
    jobs: Arc<heycode_agent::JobRegistry>,
    agent: Arc<heycode_agent::Agent>,
}

fn world(root: &std::path::Path) -> World {
    world_scripted(root, Vec::new())
}
fn world_scripted(root: &std::path::Path, scripts: Vec<Vec<heycode_llm::StreamChunk>>) -> World {
    world_configured(root, scripts, Default::default(), None)
}
fn world_configured(
    root: &std::path::Path,
    scripts: Vec<Vec<heycode_llm::StreamChunk>>,
    config: heycode_agent::ExecutionJobConfig,
    resume: Option<std::path::PathBuf>,
) -> World {
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(scripts));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        resume.map_or_else(
            || session_plugin(root.to_path_buf()),
            heycode_session::session_resume_plugin,
        ),
        prompt_plugin(),
        local_execution_plugin(
            LocalShellConfig::platform(root.to_path_buf(), Duration::from_secs(30)).unwrap(),
        ),
        terminal_registry_plugin(),
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
        agent_options_plugin(AgentOptions {
            cwd: Some(root.to_path_buf()),
            ..Default::default()
        }),
        compactions_plugin(),
        agent_plugin(),
        heycode_agent::execution_jobs_plugin_with_config(config),
    ];
    let context = compose(&plugins).unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let execution = context
        .get::<ExecutionJobService>(SERVICE_EXECUTION_JOBS)
        .unwrap();
    let jobs = context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    World {
        context,
        session,
        execution,
        jobs: (*jobs).clone(),
        agent,
    }
}

async fn wait_for_terminal(world: &World) -> heycode_exec::TerminalId {
    for _ in 0..300 {
        if let Some(status) = world
            .execution
            .terminals()
            .list(world.execution.terminal_owner())
            .await
            .into_iter()
            .next()
        {
            return status.id().clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "background terminal did not become visible: {:?}",
        world.jobs.list()
    )
}

async fn settled(world: &World, id: &heycode_agent::JobId) -> JobOutcome {
    for _ in 0..300 {
        let state = world
            .jobs
            .list()
            .into_iter()
            .find(|job| &job.id == id)
            .map(|job| job.state);
        if let Some(JobState::Settled(outcome)) = state {
            return outcome;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("background job did not settle")
}

#[cfg(unix)]
#[tokio::test]
async fn shell_and_terminal_jobs_settle_once_after_their_durable_notice() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());

    let shell = world
        .execution
        .start_shell(
            "shell-success",
            ShellRequest::new("printf shell-job").unwrap(),
            InboxDelivery::FollowUp,
        )
        .unwrap();
    assert_eq!(settled(&world, &shell).await, JobOutcome::Completed);

    let terminal = world
        .execution
        .start_terminal(
            "terminal-failure",
            ShellRequest::new("exit 7").unwrap(),
            InboxDelivery::FollowUp,
        )
        .unwrap();
    assert_eq!(settled(&world, &terminal).await, JobOutcome::Failed);

    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let notices = session
        .inbox()
        .next_turn()
        .iter()
        .chain(session.inbox().next_step())
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 2);
    assert!(notices[0].text().contains("shell exited successfully"));
    assert!(notices[0].text().contains("shell-job"));
    assert!(notices[1].text().contains("terminal exited with code 7"));
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    heycode_session::SessionEventKind::AgentInboxSplice { .. }
                )
            })
            .count(),
        2
    );
    drop(session);
    drop(world.context);
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_a_shell_job_propagates_cancellation_and_announces_once() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let id = world
        .execution
        .start_shell(
            "shell-cancel",
            ShellRequest::new("sleep 30").unwrap(),
            InboxDelivery::FollowUp,
        )
        .unwrap();

    assert!(world.jobs.cancel(&id));
    assert_eq!(settled(&world, &id).await, JobOutcome::Cancelled);
    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(session.inbox().next_turn().len(), 1);
    assert!(
        session.inbox().next_turn()[0]
            .text()
            .contains("shell cancelled")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn tasks_ps_and_stop_share_the_live_job_and_terminal_owners() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let shell = world
        .execution
        .start_shell(
            "command-shell",
            ShellRequest::new("sleep 30").unwrap(),
            InboxDelivery::FollowUp,
        )
        .unwrap();
    let terminal_job = world
        .execution
        .start_terminal(
            "command-terminal",
            ShellRequest::new("sleep 30").unwrap(),
            InboxDelivery::FollowUp,
        )
        .unwrap();
    let terminal = wait_for_terminal(&world).await;

    let tool_names = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .names();
    assert!(tool_names.contains(&"background_shell".to_owned()));
    assert!(tool_names.contains(&"background_terminal".to_owned()));

    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    world.agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        if let heycode_agent::UiEvent::Info { text } = event {
            sink.lock().unwrap().push(text.clone());
        }
    });
    let commands = world
        .context
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    for (name, args) in [
        ("tasks", "".to_owned()),
        ("ps", "".to_owned()),
        ("stop", shell.to_string()),
        ("stop", "all".to_owned()),
    ] {
        commands
            .get(name)
            .unwrap()
            .unwrap()
            .execute(&world.agent, &args)
            .await
            .unwrap();
    }

    assert_eq!(settled(&world, &shell).await, JobOutcome::Cancelled);
    assert_eq!(settled(&world, &terminal_job).await, JobOutcome::Cancelled);
    let output = seen.lock().unwrap().join("\n");
    assert!(output.contains(shell.as_str()), "{output}");
    assert!(output.contains(terminal.as_str()), "{output}");
    assert!(output.contains("command-shell"), "{output}");
    assert!(output.contains("command-terminal"), "{output}");
    let session = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(session.events().iter().all(|event| !matches!(
        event.kind,
        heycode_session::SessionEventKind::UserMessage { .. }
    )));
}

#[cfg(unix)]
#[tokio::test]
async fn output_streams_before_exit_and_terminal_tools_share_owner() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let shell = world
        .execution
        .start_shell(
            "live-output",
            ShellRequest::new("printf 'EARLY\n'; printf 'ERROR-now\n' >&2; sleep 30").unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    let output = world.execution.output(&shell).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if output
                .read(heycode_exec::OutputStream::Stdout, 0, 1024)
                .text
                .contains("EARLY")
                && output
                    .read(heycode_exec::OutputStream::Stderr, 0, 1024)
                    .text
                    .contains("ERROR-now")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!output.ended());
    assert_eq!(
        output
            .read(heycode_exec::OutputStream::Stdout, 0, 1024)
            .text,
        output
            .read(heycode_exec::OutputStream::Stdout, 0, 1024)
            .text
    );
    assert!(world.jobs.cancel(&shell));
    assert_eq!(settled(&world, &shell).await, JobOutcome::Cancelled);
    assert!(
        output
            .read(heycode_exec::OutputStream::Stderr, 0, 1024)
            .text
            .contains("ERROR-now")
    );

    let id = world
        .execution
        .start_terminal(
            "terminal-output",
            ShellRequest::new(
                "printf 'PTY-READY\n'; read answer; printf 'answer=%s\n' \"$answer\"",
            )
            .unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    let terminal = wait_for_terminal(&world).await;
    let registry = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    for name in [
        "terminal_open",
        "terminal_read",
        "terminal_write",
        "terminal_resize",
        "terminal_kill",
        "terminal_list",
        "job_output",
        "monitor",
        "run_tool",
    ] {
        assert!(registry.get(name).is_some(), "missing {name}");
    }
    let list = registry
        .get("terminal_list")
        .unwrap()
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    assert!(list.to_string().contains(terminal.as_str()));
    registry
        .get("terminal_write")
        .unwrap()
        .run(
            serde_json::json!({"terminal_id":terminal.as_str(),"input":"yes\n"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert_eq!(settled(&world, &id).await, JobOutcome::Completed);
    assert_eq!(world.execution.job_terminal(&id), Some(terminal));
    let retained =
        world
            .execution
            .output(&id)
            .unwrap()
            .read(heycode_exec::OutputStream::Terminal, 0, 4096);
    assert!(retained.text.contains("PTY-READY"), "{}", retained.text);
    assert!(retained.text.contains("answer=yes"), "{}", retained.text);
}

#[cfg(unix)]
#[tokio::test]
async fn monitor_delivers_filtered_deduplicated_events_while_source_is_alive() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let source = world
        .execution
        .start_shell(
            "watch-source",
            ShellRequest::new("printf 'noise\nERROR-one\nERROR-one\n'; sleep 30").unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    let config = heycode_agent::MonitorConfig {
        contains: "ERROR".into(),
        max_events: 1,
        ..Default::default()
    };
    let monitor = world
        .execution
        .start_monitor(&source, config, false)
        .unwrap();
    assert_eq!(settled(&world, &monitor).await, JobOutcome::Completed);
    assert!(!world.execution.output(&source).unwrap().ended());
    {
        let session = world.session.lock().unwrap();
        let events: Vec<_> = session
            .inbox()
            .next_turn()
            .iter()
            .filter(|event| event.text().contains("Monitor "))
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text().matches("ERROR-one").count(), 1);
        assert!(!events[0].text().contains("noise"));
    }
    assert!(world.jobs.cancel(&source));
    assert_eq!(settled(&world, &source).await, JobOutcome::Cancelled);
}

#[cfg(unix)]
#[tokio::test]
async fn foreground_tool_promotes_without_restarting_and_target_guard_still_runs() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let registry = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let run = registry.get("run_tool").unwrap();
    let cwd = root.path().to_path_buf();
    let handle = tokio::spawn(async move {
        run.run(serde_json::json!({"tool":"bash","arguments":{"command":"printf x >> count; sleep 30"}}),&heycode_tools::ToolCtx::default().with_cwd(cwd)).await
    });
    let id = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(id) = world.execution.foreground_jobs().first() {
                break id.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read(root.path().join("count")).unwrap_or_default() != b"x" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !handle.is_finished(),
        "foreground run_tool must wait until completion or explicit promotion"
    );
    assert!(world.execution.promote(&id));
    let result = handle.await.unwrap().unwrap();
    assert_eq!(result["job_id"], id.as_str());
    assert_eq!(result["promoted"], true);
    assert!(world.jobs.cancel(&id));
    let _outcome = settled(&world, &id).await;
    let count = std::fs::read(root.path().join("count")).unwrap_or_default();
    assert_eq!(count, b"x", "promotion restarted execution");
    assert!(world.execution.foreground_jobs().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn ordinary_model_bash_is_promotable_while_output_streams() {
    use heycode_llm::{FinishReason, StreamChunk};
    let root = tempfile::tempdir().unwrap();
    let world = world_scripted(
        root.path(),
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("ordinary-shell".into()),
                    name: Some("bash".into()),
                    arguments_delta:
                        serde_json::json!({"command":"printf ONCE >> count; printf LIVE; sleep 30"})
                            .to_string(),
                },
                StreamChunk::Finish(FinishReason::ToolCalls),
            ],
            vec![
                StreamChunk::TextDelta("continuing while the command runs".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
        ],
    );
    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("run it").await });
    let id = tokio::time::timeout(Duration::from_secs(3), async {
        'outer: loop {
            for id in world.execution.foreground_jobs() {
                if world.execution.output(&id).is_some_and(|output| {
                    output
                        .read(heycode_exec::OutputStream::Stdout, 0, 100)
                        .text
                        .contains("LIVE")
                }) {
                    break 'outer id;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !turn.is_finished(),
        "ordinary bash must keep the agent waiting until explicit promotion"
    );
    assert!(world.execution.promote(&id));
    tokio::time::timeout(Duration::from_secs(3), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!world.execution.output(&id).unwrap().ended());
    assert_eq!(std::fs::read(root.path().join("count")).unwrap(), b"ONCE");
    assert!(world.jobs.cancel(&id));
    assert_eq!(settled(&world, &id).await, JobOutcome::Cancelled);
    assert!(world.session.lock().unwrap().inbox().next_turn().iter().any(|message| matches!(message.source(), heycode_session::InboxSource::Job { job_id } if job_id == id.as_str())), "promoted jobs still deliver their result");
}

#[cfg(unix)]
#[tokio::test]
async fn foreground_shell_returns_one_tool_result_without_a_duplicate_user_message() {
    use heycode_llm::{FinishReason, StreamChunk};
    let root = tempfile::tempdir().unwrap();
    let world = world_scripted(
        root.path(),
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("single-shell".into()),
                    name: Some("bash".into()),
                    arguments_delta: serde_json::json!({"command":"printf FOREGROUND_ONLY"})
                        .to_string(),
                },
                StreamChunk::Finish(FinishReason::ToolCalls),
            ],
            vec![
                StreamChunk::TextDelta("Done".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
        ],
    );
    world.agent.send("run fixture").await.unwrap();
    let session = world.session.lock().unwrap();
    assert_eq!(session.events().iter().filter(|event| matches!(&event.kind, heycode_session::SessionEventKind::ToolResult { content, .. } if content.contains("FOREGROUND_ONLY"))).count(), 1);
    assert!(!session.events().iter().any(|event| matches!(&event.kind, heycode_session::SessionEventKind::AgentInboxSplice { inserted, .. } if inserted.iter().any(|message| matches!(message.source(), heycode_session::InboxSource::Job { .. })))));
    assert_eq!(
        world.jobs.list()[0].state,
        JobState::Settled(JobOutcome::Completed)
    );
}

struct DenyShell;
#[async_trait::async_trait]
impl heycode_core::Layer<heycode_tools::PreToolDecision> for DenyShell {
    async fn handle(
        &self,
        input: &mut heycode_tools::PreToolDecision,
        mut next: heycode_core::Next<'_, heycode_tools::PreToolDecision>,
    ) -> anyhow::Result<()> {
        if input.call.name == "bash" {
            input.verdict = heycode_tools::Verdict::Deny {
                reason: "test shell policy".into(),
            };
            Ok(())
        } else {
            next.run(input).await
        }
    }
}
#[cfg(unix)]
#[tokio::test]
async fn background_target_cannot_bypass_the_guard_or_launch_an_unsupported_tool() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    world
        .context
        .get::<heycode_core::Waterfall<heycode_tools::PreToolDecision>>(
            heycode_tools::SEAM_PRE_TOOL,
        )
        .unwrap()
        .push_shared(DenyShell);
    let registry = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let tool = registry.get("run_tool").unwrap();
    let result=tool.run(serde_json::json!({"tool":"bash","arguments":{"command":"touch forbidden"},"background":true}),&heycode_tools::ToolCtx::default()).await.unwrap();
    let id = heycode_agent::JobId::parse(result["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(settled(&world, &id).await, JobOutcome::Failed);
    assert!(!root.path().join("forbidden").exists());
    assert!(
        world
            .execution
            .output(&id)
            .unwrap()
            .read(heycode_exec::OutputStream::Stdout, 0, 1024)
            .text
            .contains("test shell policy")
    );
    assert!(
        tool.run(
            serde_json::json!({"tool":"run_tool","arguments":{},"background":true}),
            &heycode_tools::ToolCtx::default()
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn stop_acknowledges_immediately_and_does_not_fabricate_settlement_on_timeout() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let release = Arc::new(tokio::sync::Notify::new());
    let worker_release = release.clone();
    let agent = world.agent.clone();
    let jobs = world.jobs.clone();
    let id = world
        .jobs
        .spawn(
            "uncooperative fixture",
            InboxDelivery::Inject,
            move |id, _token| async move {
                worker_release.notified().await;
                agent
                    .settle_job(
                        &jobs,
                        &id,
                        &heycode_agent::JobSettlement::new(
                            JobOutcome::Completed,
                            "fixture released",
                        )
                        .unwrap(),
                    )
                    .unwrap();
            },
        )
        .unwrap();
    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = messages.clone();
    world.agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        if let heycode_agent::UiEvent::Info { text } = event {
            sink.lock().unwrap().push(text.clone());
        }
    });
    let command = world
        .context
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("stop")
        .unwrap()
        .unwrap();
    let target = id.to_string();
    let agent = world.agent.clone();
    let stop = tokio::spawn(async move { command.execute(&agent, &target).await });
    tokio::time::timeout(Duration::from_millis(200), async {
        while messages.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(messages.lock().unwrap()[0].contains("cancellation requested"));
    tokio::time::timeout(Duration::from_secs(6), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        messages
            .lock()
            .unwrap()
            .iter()
            .any(|text| text.contains("settlement is unconfirmed"))
    );
    assert!(
        world
            .jobs
            .list()
            .iter()
            .any(|row| row.id == id && row.state == JobState::Cancelling)
    );
    release.notify_one();
    assert_eq!(settled(&world, &id).await, JobOutcome::Completed);
}
#[cfg(unix)]
#[tokio::test]
async fn readiness_monitor_runs_when_all_process_slots_are_occupied_and_stops_target() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(
            world
                .execution
                .start_shell(
                    "held source",
                    ShellRequest::new("printf READY; sleep 30").unwrap(),
                    InboxDelivery::Inject,
                )
                .unwrap(),
        );
    }
    let config = heycode_agent::MonitorConfig {
        ready_when: Some("READY".into()),
        contains: "unmatched".into(),
        stop_source: true,
        ..Default::default()
    };
    // A marker can be an unterminated stream fragment; flush via newline in this fixture.
    let source = ids[0].clone();
    let monitor = world
        .execution
        .start_monitor(
            &source,
            heycode_agent::MonitorConfig {
                ready_when: Some("READY".into()),
                ..config
            },
            false,
        )
        .unwrap();
    // Partial bytes without a newline must still allow readiness recognition.
    assert_eq!(settled(&world, &monitor).await, JobOutcome::Completed);
    assert_eq!(settled(&world, &source).await, JobOutcome::Cancelled);
    let _requested = world.jobs.cancel_all();
    for id in ids {
        let _outcome = settled(&world, &id).await;
    }
}
#[cfg(unix)]
#[tokio::test]
async fn configured_output_overflow_pages_and_survives_session_resume() {
    let root = tempfile::tempdir().unwrap();
    let config = heycode_agent::ExecutionJobConfig {
        retained_bytes: 1024,
        inline_bytes: 1024,
        history_limit: 2,
        ..Default::default()
    };
    let world = world_configured(root.path(), Vec::new(), config, None);
    let id = world
        .execution
        .start_shell(
            "overflow",
            ShellRequest::new(
                "i=0; while [ $i -lt 3000 ]; do printf x; i=$((i+1)); done; printf TAIL",
            )
            .unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    assert_eq!(settled(&world, &id).await, JobOutcome::Completed);
    let output = world.execution.output(&id).unwrap();
    let page = output.read(heycode_exec::OutputStream::Stdout, 0, 2048);
    assert_eq!(page.total_bytes, 3004);
    assert_eq!(page.lost_bytes, 1980);
    assert_eq!(page.text.len(), 1024);
    assert!(page.text.ends_with("TAIL"));
    let session_path = world.session.lock().unwrap().path().to_path_buf();
    drop(output);
    drop(world);
    let resumed = world_configured(root.path(), Vec::new(), config, Some(session_path));
    let recovered = resumed.execution.output(&id).unwrap();
    assert!(recovered.ended());
    assert!(!recovered.interrupted());
    assert_eq!(
        recovered
            .read(heycode_exec::OutputStream::Stdout, 0, 2048)
            .text,
        page.text
    );
    assert!(!recovered.persistence_error());
}

struct BorrowedTool {
    name: &'static str,
    inner: Arc<dyn heycode_tools::Tool>,
}
#[async_trait::async_trait]
impl heycode_tools::Tool for BorrowedTool {
    fn spec(&self) -> heycode_core::ToolSpec {
        let mut spec = self.inner.spec();
        spec.name = self.name.to_owned();
        spec
    }
    async fn run(
        &self,
        args: serde_json::Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        self.inner.run(args, cx).await
    }
}
struct ParentOnly;
#[async_trait::async_trait]
impl heycode_tools::Tool for ParentOnly {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "parent_only".into(),
            description: "test capability".into(),
            parameters: serde_json::json!({"type":"object"}),
        }
    }
    fn supports_background(&self) -> bool {
        true
    }
    async fn run(
        &self,
        _args: serde_json::Value,
        _cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        panic!("parent-only tool escaped caller registry")
    }
}
#[cfg(unix)]
#[tokio::test]
async fn inherited_execution_tools_preserve_child_registry_cwd_inbox_and_terminal_authority() {
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir().unwrap();
    let child_dir = root.path().join("child");
    std::fs::create_dir(&child_dir).unwrap();
    let parent = world(root.path());
    let child = world(&child_dir);
    let parent_tools = parent
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let child_tools = child
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let _parent_registration = parent_tools.register_owned(Arc::new(ParentOnly)).unwrap();
    let mut registrations = Vec::new();
    for (name, source) in [
        ("borrowed_run", "run_tool"),
        ("borrowed_output", "job_output"),
        ("borrowed_shell", "background_shell"),
        ("borrowed_terminal", "background_terminal"),
        ("borrowed_terminal_write", "terminal_write"),
    ] {
        registrations.push(
            child_tools
                .register_owned(Arc::new(BorrowedTool {
                    name,
                    inner: parent_tools.get(source).unwrap(),
                }))
                .unwrap(),
        );
    }
    let result = child
        .agent
        .execute_workflow_tool(
            "borrowed_run".into(),
            serde_json::json!({"tool":"parent_only","arguments":{},"background":true}),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err(), "a child borrowed a parent-only capability");

    let response = child
        .agent
        .execute_workflow_tool(
            "borrowed_shell".into(),
            serde_json::json!({"command":"pwd; printf child > marker"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let child_job = heycode_agent::JobId::parse(response["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(settled(&parent, &child_job).await, JobOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(child_dir.join("marker")).unwrap(),
        "child"
    );
    assert!(!root.path().join("marker").exists());
    assert!(
        parent
            .session
            .lock()
            .unwrap()
            .inbox()
            .next_turn()
            .is_empty()
    );
    assert_eq!(
        child.session.lock().unwrap().inbox().next_turn().len(),
        1,
        "settlement belongs in caller inbox"
    );
    let output = child
        .agent
        .execute_workflow_tool(
            "borrowed_output".into(),
            serde_json::json!({"job_id":child_job.as_str()}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output["page"]["text"].as_str().unwrap().contains("child"));

    let parent_job = parent
        .execution
        .start_shell(
            "private parent output",
            ShellRequest::new("printf parent-secret").unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    assert_eq!(settled(&parent, &parent_job).await, JobOutcome::Completed);
    assert!(
        child
            .agent
            .execute_workflow_tool(
                "borrowed_output".into(),
                serde_json::json!({"job_id":parent_job.as_str()}),
                CancellationToken::new()
            )
            .await
            .is_err()
    );

    let response = child
        .agent
        .execute_workflow_tool(
            "borrowed_terminal".into(),
            serde_json::json!({"command":"read answer; printf 'child:%s' \"$answer\""}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let terminal_job = heycode_agent::JobId::parse(response["job_id"].as_str().unwrap()).unwrap();
    let terminal = parent.execution.job_terminal(&terminal_job).unwrap();
    assert!(
        parent
            .execution
            .terminals()
            .write(parent.execution.terminal_owner(), &terminal, b"wrong\n")
            .await
            .is_err()
    );
    child
        .agent
        .execute_workflow_tool(
            "borrowed_terminal_write".into(),
            serde_json::json!({"terminal_id":terminal.as_str(),"input":"yes\n"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(settled(&parent, &terminal_job).await, JobOutcome::Completed);
    assert!(
        parent
            .execution
            .output(&terminal_job)
            .unwrap()
            .read(heycode_exec::OutputStream::Terminal, 0, 1024)
            .text
            .contains("child:yes")
    );
    let response = child
        .agent
        .execute_workflow_tool(
            "borrowed_shell".into(),
            serde_json::json!({"command":"printf alive; sleep 30"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let held = heycode_agent::JobId::parse(response["job_id"].as_str().unwrap()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while parent
            .execution
            .output(&held)
            .unwrap()
            .read(heycode_exec::OutputStream::Stdout, 0, 1024)
            .text
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let child_session = child.session.lock().unwrap().path().to_path_buf();
    child.agent.token().shutdown();
    assert_eq!(settled(&parent, &held).await, JobOutcome::Cancelled);
    drop(child_tools);
    drop(child);
    // Settlement is durable before the worker future finishes unwinding. Parent
    // observations must not retain the writer after that owned tail is released.
    let _reopened = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match Session::open_for_writing(child_session.parent().unwrap()) {
                Ok(session) => break session,
                Err(heycode_session::OpenError::AlreadyOpen) => tokio::task::yield_now().await,
                Err(error) => panic!("failed to reopen settled child session: {error}"),
            }
        }
    })
    .await
    .expect("child writer must be released after worker teardown");
    drop(registrations);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn rebound_execution_wrappers_enforce_child_process_sandbox() {
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir().unwrap();
    let child_dir = root.path().join("child");
    std::fs::create_dir(&child_dir).unwrap();
    let parent = world(root.path());
    let child = world(&child_dir);
    let sandbox = heycode_exec::SandboxService::new(
        heycode_exec::SandboxMode::WorkspaceWrite,
        &child_dir,
        Some(heycode_sandbox::platform_default().unwrap()),
    )
    .unwrap();
    let filesystem = heycode_exec::FileSystemService::local(
        heycode_exec::FileSystemPolicy::from_sandbox(sandbox.policy()).unwrap(),
    )
    .unwrap();
    // The production worktree path preserves parent shell defaults and replaces executor.
    let shell = parent
        .context
        .get::<heycode_exec::ShellService>(heycode_exec::SERVICE_SHELL)
        .unwrap()
        .with_executor(heycode_exec::SubprocessService::local_with_sandbox(sandbox));
    let parent_tools = parent
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let rebound = parent_tools.for_workspace(&filesystem, &shell).unwrap();
    let child_tools = child
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let mut registrations = Vec::new();
    for (name, source) in [
        ("rebound_shell", "background_shell"),
        ("rebound_terminal", "background_terminal"),
        ("rebound_monitor", "monitor"),
        ("rebound_open", "terminal_open"),
    ] {
        registrations.push(
            child_tools
                .register_owned(Arc::new(BorrowedTool {
                    name,
                    inner: rebound.get(source).unwrap(),
                }))
                .unwrap(),
        );
    }
    for (index, name) in [
        "rebound_shell",
        "rebound_terminal",
        "rebound_monitor",
        "rebound_open",
    ]
    .into_iter()
    .enumerate()
    {
        let forbidden = root.path().join(format!("forbidden-{index}"));
        let allowed = child_dir.join(format!("allowed-{index}"));
        let command = format!(
            "printf allowed > allowed-{index}; printf forbidden > '{}'; printf DONE",
            forbidden.display()
        );
        let args = if name == "rebound_open" {
            serde_json::json!({"command":"sh","args":["-c",command]})
        } else if name == "rebound_monitor" {
            serde_json::json!({"command":command,"watch":{"ready_when":"DONE","timeout_ms":3000}})
        } else {
            serde_json::json!({"command":command})
        };
        let response = child
            .agent
            .execute_workflow_tool(name.into(), args, CancellationToken::new())
            .await
            .unwrap();
        if name != "rebound_open" {
            let id = heycode_agent::JobId::parse(response["job_id"].as_str().unwrap()).unwrap();
            assert_eq!(settled(&parent, &id).await, JobOutcome::Completed, "{name}");
        } else {
            let terminal =
                heycode_exec::TerminalId::parse(response["terminal_id"].as_str().unwrap()).unwrap();
            let exit = tokio::time::timeout(
                Duration::from_secs(3),
                parent.execution.terminals().wait(
                    child.execution.terminal_owner(),
                    &terminal,
                    CancellationToken::new(),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(exit.is_success(), "{name} did not settle successfully");
        }
        assert_eq!(
            std::fs::read_to_string(allowed).unwrap(),
            "allowed",
            "{name}"
        );
        assert!(!forbidden.exists(), "{name} escaped child sandbox");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unlimited_monitor_wakes_and_cancellation_preserve_source_ownership() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    for _ in 0..10 {
        assert!(world.jobs.reserve_event_wake());
    }
    let source = world
        .execution
        .start_shell(
            "held",
            ShellRequest::new("printf 'EVENT\n'; sleep 30").unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    let monitor = world
        .execution
        .start_monitor(
            &source,
            heycode_agent::MonitorConfig {
                debounce_ms: 100,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while world.session.lock().unwrap().inbox().next_turn().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(world.jobs.cancel(&monitor));
    assert_eq!(settled(&world, &monitor).await, JobOutcome::Cancelled);
    assert!(
        !world.execution.output(&source).unwrap().ended(),
        "existing source remains independent"
    );
    let owned = world
        .execution
        .start_monitor(
            &source,
            heycode_agent::MonitorConfig {
                contains: "never-match".into(),
                ..Default::default()
            },
            true,
        )
        .unwrap();
    assert!(world.jobs.cancel(&owned));
    assert_eq!(settled(&world, &owned).await, JobOutcome::Cancelled);
    assert_eq!(settled(&world, &source).await, JobOutcome::Cancelled);
    assert_eq!(
        world.session.lock().unwrap().inbox().next_turn().len(),
        1,
        "cancelled watch must not deliver another event"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_tool_defaults_to_foreground_without_a_duplicate_inbox_notice() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let registry = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let run = registry.get("run_tool").unwrap();
    let context = heycode_tools::ToolCtx::default().with_cwd(root.path().to_path_buf());
    let result = run
        .run(
            serde_json::json!({"tool":"bash","arguments":{"command":"printf DIRECT_RESULT"}}),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(result["outcome"], "completed");
    assert!(
        result["output"]["text"]
            .as_str()
            .unwrap()
            .contains("DIRECT_RESULT")
    );
    assert!(world.agent.pending_inbox().is_empty());
    let background = run
        .run(
            serde_json::json!({"tool":"bash","arguments":{"command":"sleep 30"},"background":true}),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(background["background"], true);
    let job = heycode_agent::JobId::parse(background["job_id"].as_str().unwrap()).unwrap();
    assert!(!world.execution.output(&job).unwrap().ended());
    assert!(world.jobs.cancel(&job));
    assert_eq!(settled(&world, &job).await, JobOutcome::Cancelled);
    assert_eq!(world.agent.pending_inbox().next_turn, 1);
}

#[cfg(unix)]
#[tokio::test]
async fn background_jobs_outlive_foreground_deadline_and_retain_execution_facts() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let shell = heycode_exec::ShellService::local(
        LocalShellConfig::platform(root.path().to_path_buf(), Duration::from_millis(20)).unwrap(),
    );
    assert_eq!(
        shell
            .resolve(ShellRequest::new("true").unwrap())
            .unwrap()
            .timeout(),
        Some(Duration::from_millis(20))
    );
    let command = "sleep 0.2; printf FINISHED";
    let shell_id = world
        .execution
        .start_shell_with_shell(
            &shell,
            "longer shell",
            ShellRequest::new(command).unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    let terminal_id = world
        .execution
        .start_terminal_with_shell(
            &shell,
            "longer terminal",
            ShellRequest::new(command).unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    for id in [&shell_id, &terminal_id] {
        assert_eq!(settled(&world, id).await, JobOutcome::Completed);
        let output = world.execution.output(id).unwrap();
        let metadata = output.metadata().unwrap();
        assert_eq!(metadata.command, command);
        assert_eq!(metadata.cwd, root.path().display().to_string());
        assert_eq!(metadata.timeout_ms, None);
        assert!(metadata.elapsed_ms >= 180, "{metadata:?}");
        assert!(metadata.finished_ms.unwrap() >= metadata.started_ms.unwrap());
        assert!(metadata.reason.as_ref().unwrap().contains("successfully"));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(output.metadata().unwrap().elapsed_ms, metadata.elapsed_ms);
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "real two-minute runtime verification; run explicitly for the parity audit"]
async fn parity_two_minute_background_terminal_completes_twelve_ticks() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let id = world.execution.start_terminal(
        "two-minute ticker",
        ShellRequest::new("i=0; while [ $i -lt 12 ]; do sleep 10; i=$((i+1)); printf 'TICK_%s\n' \"$i\"; done").unwrap(),
        InboxDelivery::Inject,
    ).unwrap();
    tokio::time::timeout(Duration::from_secs(135), async {
        loop {
            if world
                .jobs
                .list()
                .iter()
                .any(|job| job.id == id && matches!(job.state, JobState::Settled(_)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(settled(&world, &id).await, JobOutcome::Completed);
    let output = world.execution.output(&id).unwrap();
    let text = output
        .read(heycode_exec::OutputStream::Terminal, 0, 4096)
        .text;
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("TICK_"))
            .count(),
        12,
        "{text}"
    );
    assert!(text.contains("TICK_12"));
    assert!(output.metadata().unwrap().elapsed_ms >= 120_000);
    assert_eq!(output.metadata().unwrap().timeout_ms, None);
}

#[cfg(unix)]
#[tokio::test]
async fn foreground_tool_automatically_promotes_once_without_restarting() {
    let root = tempfile::tempdir().unwrap();
    let world = world_configured(
        root.path(),
        Vec::new(),
        heycode_agent::ExecutionJobConfig {
            foreground_timeout_secs: 1,
            ..Default::default()
        },
        None,
    );
    let registry = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let run = registry.get("run_tool").unwrap();
    let cwd = root.path().to_path_buf();
    let handle = tokio::spawn(async move {
        run.run(serde_json::json!({"tool":"bash", "arguments":{"command":"printf x >> count; sleep 2; printf AUTO_DONE"}}), &heycode_tools::ToolCtx::default().with_cwd(cwd)).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !handle.is_finished(),
        "ordinary tools start in the foreground"
    );
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["background"], true);
    assert_eq!(result["automatic"], true);
    let id = heycode_agent::JobId::parse(result["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(settled(&world, &id).await, JobOutcome::Completed);
    assert_eq!(std::fs::read(root.path().join("count")).unwrap(), b"x");
    assert!(
        world
            .execution
            .output(&id)
            .unwrap()
            .read(heycode_exec::OutputStream::Stdout, 0, 4096)
            .text
            .contains("AUTO_DONE")
    );
    assert_eq!(world.jobs.list().len(), 1);
}

struct SlowBackgroundTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl heycode_tools::Tool for SlowBackgroundTool {
    fn supports_background(&self) -> bool {
        true
    }
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "slow_connected_tool".into(),
            description: "Controlled delayed tool".into(),
            parameters: serde_json::json!({"type":"object"}),
        }
    }
    async fn run(
        &self,
        _: serde_json::Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::select! {
            () = self.release.notified() => Ok(serde_json::json!({"result":"RETAINED_CONNECTED_RESULT"})),
            () = cx.cancellation.cancelled() => Err(heycode_tools::ToolError::new("cancelled")),
        }
    }
}

#[tokio::test]
async fn direct_connected_tool_auto_handoff_preserves_output_and_frees_parent() {
    use heycode_llm::{FinishReason, StreamChunk};
    let root = tempfile::tempdir().unwrap();
    let world = world_configured(
        root.path(),
        vec![
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some("direct-connected".into()),
                    name: Some("slow_connected_tool".into()),
                    arguments_delta: "{}".into(),
                },
                StreamChunk::Finish(FinishReason::ToolCalls),
            ],
            vec![
                StreamChunk::TextDelta("PARENT_CAN_CONTINUE".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
            vec![
                StreamChunk::TextDelta("BACKGROUND_COMPLETED".into()),
                StreamChunk::Finish(FinishReason::Stop),
            ],
        ],
        heycode_agent::ExecutionJobConfig {
            foreground_timeout_secs: 1,
            ..Default::default()
        },
        None,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .register_shared(Arc::new(SlowBackgroundTool {
            calls: calls.clone(),
            release: release.clone(),
        }))
        .unwrap();
    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("run connected operation").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!turn.is_finished());
    let reply = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.text, "PARENT_CAN_CONTINUE");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let jobs = world.jobs.list();
    assert_eq!(jobs.len(), 1);
    assert!(matches!(
        jobs[0].state,
        JobState::Running | JobState::Queued
    ));
    release.notify_one();
    assert_eq!(settled(&world, &jobs[0].id).await, JobOutcome::Completed);
    assert!(
        world
            .execution
            .output(&jobs[0].id)
            .unwrap()
            .read(heycode_exec::OutputStream::Stdout, 0, 4096)
            .text
            .contains("RETAINED_CONNECTED_RESULT")
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn canonical_job_controls_validate_before_effects_and_keep_output_compatibility() {
    let root = tempfile::tempdir().unwrap();
    let world = world(root.path());
    let tools = world
        .context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let control = tools.get("job_control").unwrap();
    assert_eq!(
        control.spec().parameters["required"],
        serde_json::json!(["action"])
    );
    for legacy in ["list_jobs", "cancel_job", "job_output"] {
        assert!(tools.get(legacy).is_some());
        assert!(!tools.advertised_names().contains(&legacy.to_owned()));
    }
    let id = world
        .execution
        .start_shell(
            "held output",
            ShellRequest::new("printf READY; sleep 30").unwrap(),
            InboxDelivery::Inject,
        )
        .unwrap();
    let cx = heycode_tools::ToolCtx::default();
    for args in [
        serde_json::json!({"job_id":id.as_str()}),
        serde_json::json!({"action":"invalid","job_id":id.as_str()}),
        serde_json::json!({"action":"cancel","job_id":id.as_str(),"stream":"stdout"}),
        serde_json::json!({"action":"cancel","job_id":"agent-1"}),
    ] {
        assert!(control.run(args, &cx).await.is_err());
    }
    assert!(
        world
            .jobs
            .list()
            .iter()
            .any(|job| job.id == id && matches!(job.state, JobState::Running | JobState::Queued))
    );
    assert!(
        control
            .run(serde_json::json!({"action":"list"}), &cx)
            .await
            .unwrap()["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["id"] == id.as_str())
    );
    let receipt = control
        .run(
            serde_json::json!({"action":"cancel","job_id":id.as_str()}),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(receipt["status"], "requested");
    assert_eq!(settled(&world, &id).await, JobOutcome::Cancelled);
    let output = control
        .run(
            serde_json::json!({"action":"output","job_id":id.as_str()}),
            &cx,
        )
        .await
        .unwrap();
    let legacy = tools
        .get("job_output")
        .unwrap()
        .run(serde_json::json!({"job_id":id.as_str()}), &cx)
        .await
        .unwrap();
    assert_eq!(output["page"], legacy["page"]);
    assert_eq!(output["ended"], true);
    assert_eq!(
        control
            .run(
                serde_json::json!({"action":"cancel","job_id":id.as_str()}),
                &cx
            )
            .await
            .unwrap()["status"],
        "not_running"
    );
}

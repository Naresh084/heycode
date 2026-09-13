//! Task console state, ownership, input and real renderer interaction.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use heycode_tui::{
    app::{AppState, accessibility::ScreenReaderSnapshot},
    task_console::*,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Source {
    rows: Mutex<Vec<TaskRecord>>,
    fail_read: Mutex<bool>,
    page: Mutex<Option<TaskOutputPage>>,
}

#[async_trait]
impl TaskSource for Source {
    fn snapshot(&self) -> Result<Vec<TaskRecord>, String> {
        if *self.fail_read.lock().unwrap() {
            Err("owner disconnected".into())
        } else {
            Ok(self.rows.lock().unwrap().clone())
        }
    }
    fn output(
        &self,
        _: &TaskKey,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TaskOutputPage, String> {
        if let Some(page) = self.page.lock().unwrap().as_ref() {
            return Ok(page.clone());
        }
        let end = before.unwrap_or(401).min(401);
        let start = end.saturating_sub(limit as u64).max(1);
        Ok(TaskOutputPage {
            events: (start..end)
                .map(|sequence| TaskOutputEvent {
                    sequence,
                    kind: TaskOutputKind::Text(format!("output {sequence}")),
                })
                .collect(),
            has_older: start > 1,
            ..TaskOutputPage::default()
        })
    }
    async fn execute(&self, _: TaskAction, _: CancellationToken) -> Result<String, String> {
        Ok("accepted".into())
    }
}

fn row(id: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        key: TaskKey(format!("child:{id}")),
        kind: TaskKind::Child,
        label: format!("Child {id}"),
        status,
        parent: Some("root".into()),
        session: Some(format!("session-{id}")),
        job: None,
        capabilities: TaskCapabilities {
            output: true,
            steer: true,
            terminal_input: false,
            interrupt: status.active(),
            close: true,
            background: false,
            retry: false,
        },
        telemetry: TaskTelemetry {
            elapsed_ms: Some(1200),
            current_tool: Some("read".into()),
            input_tokens: Some(45),
            output_tokens: Some(12),
            ..TaskTelemetry::default()
        },
        detail: None,
    }
}

fn state() -> (AppState, Arc<Source>) {
    let source = Arc::new(Source {
        rows: Mutex::new(vec![
            row("a", TaskStatus::Running),
            row("b", TaskStatus::Waiting),
            row("c", TaskStatus::Completed),
        ]),
        ..Source::default()
    });
    let mut state = AppState::new("native-model", "/workspace".into());
    state.set_task_source(source.clone());
    (state, source)
}

fn press(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!state.handle_terminal_event(&Event::Key(KeyEvent::new(code, modifiers))));
}

fn frame(state: &mut AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn empty_task_inventory_uses_no_strip_but_the_task_shortcut_still_opens() {
    let (mut state, source) = state();
    source.rows.lock().unwrap().clear();
    state.refresh_tasks();
    assert!(!text(&frame(&mut state, 120, 25)).contains("Agents 0"));
    source
        .rows
        .lock()
        .unwrap()
        .push(row("new", TaskStatus::Running));
    state.refresh_tasks();
    assert!(text(&frame(&mut state, 120, 25)).contains("1 agent"));
    source.rows.lock().unwrap().clear();
    state.refresh_tasks();
    press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(text(&frame(&mut state, 120, 25)).contains("No tasks currently running"));
}

#[test]
fn task_console_persistent_strip_counts_are_authoritative_and_accessible() {
    let (mut state, _) = state();
    state.apply(&heycode_agent::UiEvent::AssistantDelta {
        text: "I started 99 agents".into(),
    });
    let full = text(&frame(&mut state, 120, 24));
    assert!(
        full.contains("2 agents") && !full.contains("Ctrl+T browse"),
        "{full}"
    );
    let flat = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("Agents 2 · 2 active"), "{flat}");
    assert_eq!(
        state.task_console().records.len(),
        3,
        "model prose cannot mint registry tasks"
    );
    for (width, height) in [(35, 10), (80, 15), (120, 24)] {
        let full = text(&frame(&mut state, width, height));
        assert!(full.contains("2 agents"), "{width}x{height}: {full}");
        assert!(!full.contains("Ctrl+T"), "{full}");
    }
}

#[test]
fn completed_tool_history_does_not_inflate_the_task_strip() {
    let (mut state, source) = state();
    let mut completed = row("old-tool", TaskStatus::Completed);
    completed.kind = heycode_tui::task_console::TaskKind::Tool;
    *source.rows.lock().unwrap() = vec![completed];
    state.refresh_tasks();
    assert!(!text(&frame(&mut state, 120, 25)).contains("Agents 1"));
    press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(
        !text(&frame(&mut state, 120, 25)).contains("old-tool"),
        "foreground tool history stays in the main transcript"
    );
}

#[test]
fn task_console_switches_three_children_without_losing_parent_or_child_drafts() {
    let (mut state, _) = state();
    state.input.insert_str("parent line one\nparent line two");
    let cursor = state.input.cursor();
    state.pending_send = Some("already queued for parent".into());
    for id in ["a", "b", "c"] {
        state.open_task(TaskKey(format!("child:{id}")));
        assert_eq!(state.input.lines(), &[String::new()]);
        state.input.insert_str(format!("draft for {id}"));
        // Parent streams while the child is visible, without contaminating it.
        state.apply(&heycode_agent::UiEvent::AssistantDelta {
            text: format!("parent update {id}"),
        });
        let flat = ScreenReaderSnapshot::from_state(&state).into_text();
        assert!(
            !flat.contains("parent update"),
            "parent transcript should not be painted in child view"
        );
        press(&mut state, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(state.input.lines(), &["parent line one", "parent line two"]);
        assert_eq!(state.input.cursor(), cursor);
    }
    state.open_task(TaskKey("child:a".into()));
    assert_eq!(state.input.lines(), &["draft for a"]);
    state.open_task_category(TaskCategory::Agents);
    assert_eq!(state.task_console().view, ConsoleView::List);
    assert_eq!(state.input.lines(), &["draft for a"]);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(state.task_console().view, ConsoleView::Detail);
    press(&mut state, KeyCode::Left, KeyModifiers::ALT);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(state.task_console().view, ConsoleView::Collapsed);
    assert_eq!(state.input.lines(), &["parent line one", "parent line two"]);
    assert_eq!(
        state.pending_send.as_deref(),
        Some("already queued for parent")
    );
}

#[test]
fn task_console_selection_survives_reordering_and_unknown_tasks_cannot_receive_input() {
    let (mut state, source) = state();
    state.open_task(TaskKey("child:b".into()));
    source.rows.lock().unwrap().reverse();
    state.refresh_tasks();
    assert_eq!(
        state.task_console().selected_record().unwrap().key.0,
        "child:b"
    );
    source
        .rows
        .lock()
        .unwrap()
        .retain(|row| row.key.0 != "child:b");
    state.refresh_tasks();
    state.input.insert_str("do not lose this");
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.input.lines(), &["do not lose this"]);
    assert!(
        state
            .task_console()
            .notice
            .as_ref()
            .unwrap()
            .contains("no longer")
    );
    assert!(
        state.pending_send.is_none(),
        "failed child submission must never fall through to parent"
    );
}

#[test]
fn task_console_mouse_strip_and_task_rows_use_actual_frame_hit_regions() {
    let (mut state, _) = state();
    frame(&mut state, 100, 24);
    let click = |column, row| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let strip = state
        .task_console()
        .hits
        .iter()
        .find(|(_, hit)| *hit == TaskHit::Category(TaskCategory::Agents))
        .unwrap()
        .0;
    state.handle_terminal_event(&click(strip.x, strip.y));
    assert_eq!(state.task_console().view, ConsoleView::List);
    frame(&mut state, 100, 24);
    let child = state
        .task_console()
        .hits
        .iter()
        .find(|(_, hit)| *hit == TaskHit::Open(TaskKey("child:a".into())))
        .unwrap()
        .0;
    state.handle_terminal_event(&click(child.x, child.y));
    assert_eq!(state.task_console().view, ConsoleView::Detail);
    assert_eq!(state.task_console().selected.as_ref().unwrap().0, "child:a");
}

#[test]
fn task_console_pages_output_without_repeating_or_skipping_source_cursors() {
    let source = Arc::new(Source::default());
    let mut console = TaskConsole::default();
    source
        .rows
        .lock()
        .unwrap()
        .push(row("a", TaskStatus::Running));
    console.attach(source);
    console.open_selected();
    let latest = console.page.events.clone();
    assert_eq!(latest.last().unwrap().sequence, 400);
    console.older_page();
    assert_eq!(
        console.page.events.last().unwrap().sequence + 1,
        latest.first().unwrap().sequence
    );
    assert!(!console.follow);
    let previous = console.page.clone();
    console.refresh();
    assert_eq!(
        console.page, previous,
        "live refresh must not move a page under the reader"
    );
    console.newer_page();
    assert_eq!(console.page.events, latest);
    assert!(console.follow);
}

#[test]
fn task_console_does_not_represent_requested_cancellation_as_settled() {
    let (mut state, source) = state();
    state.open_task(TaskKey("child:a".into()));
    press(&mut state, KeyCode::Char('i'), KeyModifiers::ALT);
    assert_eq!(
        state.task_console().selected_record().unwrap().status,
        TaskStatus::Running
    );
    source.rows.lock().unwrap()[0].status = TaskStatus::Cancelling;
    state.refresh_tasks();
    assert_eq!(
        state.task_console().selected_record().unwrap().status,
        TaskStatus::Cancelling
    );
    source.rows.lock().unwrap()[0].status = TaskStatus::Cancelled;
    state.refresh_tasks();
    assert_eq!(
        state.task_console().selected_record().unwrap().status,
        TaskStatus::Cancelled
    );
}

#[test]
fn task_console_owner_read_failure_preserves_inventory_with_explicit_staleness() {
    let (mut state, source) = state();
    *source.fail_read.lock().unwrap() = true;
    state.refresh_tasks();
    assert_eq!(state.task_console().records.len(), 3);
    assert!(
        state
            .task_console()
            .inventory_error
            .as_ref()
            .unwrap()
            .contains("owner disconnected")
    );
}

struct HeldProvider {
    release: Arc<tokio::sync::Notify>,
    entered: std::sync::atomic::AtomicUsize,
}

impl heycode_llm::Provider for HeldProvider {
    fn info(&self) -> heycode_llm::ProviderInfo {
        heycode_llm::testing::FakeProvider::new(Vec::new()).info()
    }
    fn stream(&self, _: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
        use futures::StreamExt;
        let index = self
            .entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let release = self.release.clone();
        Box::pin(
            futures::stream::iter(vec![
                Ok(heycode_llm::StreamChunk::TextDelta(format!(
                    "live child text {index}"
                ))),
                Ok(heycode_llm::StreamChunk::ReasoningDelta(format!(
                    "reasoning {index}"
                ))),
            ])
            .chain(futures::stream::once(async move {
                release.notified().await;
                Ok(heycode_llm::StreamChunk::Finish(
                    heycode_llm::FinishReason::Stop,
                ))
            })),
        )
    }
}

fn native_world(
    root: &std::path::Path,
    provider: Arc<dyn heycode_llm::Provider>,
) -> heycode_core::Context {
    let cwd = root.canonicalize().unwrap();
    heycode_core::compose(&[
        heycode_session::session_plugin(cwd.clone()),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::terminal_registry_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            heycode_llm::LlmSelection {
                provider_name: "fake".into(),
                model: "native-fixture".into(),
            },
            vec![provider],
        ),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::AutoApprove)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::subagent_plugin(cwd.clone(), 3),
        heycode_agent::agent_options_plugin(heycode_agent::AgentOptions {
            cwd: Some(cwd),
            ..heycode_agent::AgentOptions::default()
        }),
        heycode_agent::agent_plugin(),
        heycode_agent::execution_jobs_plugin(),
        heycode_agent::subagent_jobs_plugin(),
    ])
    .unwrap()
}

#[tokio::test]
async fn task_console_native_three_children_stream_before_settlement_then_interrupt_and_close() {
    let root = tempfile::tempdir().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(HeldProvider {
        release: release.clone(),
        entered: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut context = native_world(root.path(), provider.clone());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let registry = context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let jobs = (*context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap())
    .clone();
    let authority = registry.root_authority(
        heycode_agent::SubagentId::new(agent.session().lock().unwrap().id().as_str()).unwrap(),
    );
    let source = Arc::new(
        RegistryTaskSource::new(agent, Some(jobs.clone()), Some(registry.clone())).unwrap(),
    );
    let mut state = AppState::new("native-fixture", root.path().into());
    state.set_task_source(source.clone());
    state.input.insert_str("preserve parent draft");
    let mut children = Vec::new();
    for label in ["alpha", "beta", "gamma"] {
        let request = heycode_agent::SubagentRequest::with_authority(
            label,
            format!("work {label}"),
            heycode_agent::SubagentSeed::Fresh,
            heycode_agent::SubagentContinuation::Continuable,
            authority.clone(),
        )
        .unwrap();
        let (id, _) = registry
            .start_background_task(request, heycode_session::InboxDelivery::Inject)
            .unwrap();
        children.push(id);
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if children.iter().all(|id| source.output(&TaskKey(format!("child:{}", id.as_str())), None, 128).is_ok_and(|page| page.events.iter().any(|event| matches!(&event.kind, TaskOutputKind::Text(text) if text.contains("live child text"))))) { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("all three native child streams must be visible before their provider finishes");
    state.refresh_tasks();
    assert_eq!(
        state
            .task_console()
            .records
            .iter()
            .filter(|r| r.kind == TaskKind::Child && r.status == TaskStatus::Running)
            .count(),
        3
    );
    for id in &children {
        state.open_task(TaskKey(format!("child:{}", id.as_str())));
        let flat = ScreenReaderSnapshot::from_state(&state).into_text();
        assert!(flat.contains("live child text"), "{flat}");
        assert!(flat.contains("reasoning"), "{flat}");
        assert!(
            state
                .task_console()
                .selected_record()
                .unwrap()
                .telemetry
                .elapsed_ms
                .is_some()
        );
        state.return_from_task();
        assert_eq!(state.input.lines(), &["preserve parent draft"]);
    }
    let first = TaskKey(format!("child:{}", children[0].as_str()));
    let previous_jobs = jobs.list();
    source
        .execute(
            TaskAction::Steer {
                key: first.clone(),
                text: "resume alpha after interruption".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let queued_job = jobs
        .list()
        .into_iter()
        .find(|job| !previous_jobs.iter().any(|previous| previous.id == job.id))
        .expect("steering reserves an owned message job");
    let native = registry.native_child_for(&authority, &children[0]).unwrap();
    let pending = native.next_wakeable_message().unwrap();
    let ack = source
        .execute(
            TaskAction::Interrupt(first.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(ack.contains("requested"));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if source
                .snapshot()
                .unwrap()
                .iter()
                .any(|row| row.key == first && row.status == TaskStatus::Cancelled)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        jobs.wait_for_settlement(&queued_job.id).await.unwrap();
    })
    .await
    .expect("interruption must settle without restarting queued input");
    assert_eq!(
        provider.entered.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    assert_eq!(native.next_wakeable_message(), Some(pending.clone()));
    assert!(!native.token().is_turn_active());
    assert!(registry.child_for(&authority, &children[0]).is_some());

    // Explicit restoration permits resuming the exact retained occurrence.
    assert!(
        registry
            .archive_child_for(&authority, &children[0])
            .unwrap()
    );
    assert!(
        registry
            .restore_child_for(&authority, &children[0])
            .unwrap()
    );
    assert_eq!(
        provider.entered.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    let child = registry.child_for(&authority, &children[0]).unwrap();
    let resumed =
        tokio::spawn(async move { child.run_pending(&pending, CancellationToken::new()).await });
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !source.output(&first, None, 128).unwrap().events.iter().any(|event| {
            matches!(&event.kind, TaskOutputKind::Text(text) if text.contains("live child text 3"))
        }) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("explicit resume must consume the retained input");
    assert!(native.pending_human_messages().is_empty());
    source
        .execute(
            TaskAction::Interrupt(first.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), resumed)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), heycode_agent::SubagentErrorCode::Cancelled);
    assert_eq!(
        provider.entered.load(std::sync::atomic::Ordering::SeqCst),
        4
    );
    assert_eq!(
        source
            .snapshot()
            .unwrap()
            .iter()
            .find(|row| row.key == first)
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );
    let claimed_count = native
        .session()
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter(|event| {
            matches!(&event.kind, heycode_session::SessionEventKind::UserMessage { text }
            if text == "resume alpha after interruption")
        })
        .count();
    assert_eq!(claimed_count, 1);
    release.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if source
                .snapshot()
                .unwrap()
                .iter()
                .filter(|r| r.kind == TaskKind::Child)
                .all(|r| !r.status.active())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let second = TaskKey(format!("child:{}", children[1].as_str()));
    source
        .execute(TaskAction::Close(second.clone()), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        source
            .snapshot()
            .unwrap()
            .iter()
            .find(|r| r.key == second)
            .unwrap()
            .status,
        TaskStatus::Closed
    );
    assert!(source.output(&second, None, 128).unwrap().events.iter().any(|e| matches!(&e.kind, TaskOutputKind::Text(text) if text.contains("live child text"))), "settled output remains readable after close");
    context.shutdown();
}

#[cfg(unix)]
#[tokio::test]
async fn task_console_process_output_and_terminal_input_are_live_and_retained() {
    let root = tempfile::tempdir().unwrap();
    let mut context = native_world(
        root.path(),
        Arc::new(heycode_llm::testing::FakeProvider::new(Vec::new())),
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let execution = context
        .get::<heycode_agent::ExecutionJobService>(heycode_agent::SERVICE_EXECUTION_JOBS)
        .unwrap();
    let source = RegistryTaskSource::new(agent, Some(execution.jobs().clone()), None)
        .unwrap()
        .with_execution(Some(execution.clone()));
    let id = execution
        .start_terminal(
            "interactive echo",
            heycode_exec::ShellRequest::new(
                "printf 'PTY_READY\\n'; IFS= read -r line; printf 'PTY_INPUT:%s\\n' \"$line\"",
            )
            .unwrap(),
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    let key = TaskKey(format!("job:{id}"));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if source.output(&key, None, 128).is_ok_and(|page| page.events.iter().any(|event| matches!(&event.kind, TaskOutputKind::Text(text) if text.contains("PTY_READY")))) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let rows = source.snapshot().unwrap();
    let row = rows.iter().find(|row| row.key == key).unwrap();
    assert_eq!(row.status, TaskStatus::Running);
    assert!(row.capabilities.terminal_input);
    source
        .execute(
            TaskAction::TerminalInput {
                key: key.clone(),
                text: "hello from task composer".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        execution.jobs().wait_for_settlement(&id),
    )
    .await
    .unwrap()
    .unwrap();
    let page = source.output(&key, None, 128).unwrap();
    assert!(page.events.iter().any(|event| matches!(&event.kind, TaskOutputKind::Text(text) if text.contains("PTY_INPUT:hello from task composer"))));
    assert_eq!(
        source.output(&key, None, 128).unwrap(),
        page,
        "UI reads never consume process output"
    );
    assert!(
        !source
            .snapshot()
            .unwrap()
            .iter()
            .find(|row| row.key == key)
            .unwrap()
            .capabilities
            .terminal_input
    );
    assert!(
        source
            .execute(
                TaskAction::TerminalInput {
                    key,
                    text: "late".into()
                },
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    context.shutdown();
}

#[cfg(unix)]
#[tokio::test]
async fn task_console_promotes_exact_foreground_job_and_keeps_independent_output_streams() {
    let root = tempfile::tempdir().unwrap();
    let mut context = native_world(
        root.path(),
        Arc::new(heycode_llm::testing::FakeProvider::new(Vec::new())),
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let execution = context
        .get::<heycode_agent::ExecutionJobService>(heycode_agent::SERVICE_EXECUTION_JOBS)
        .unwrap();
    let source = RegistryTaskSource::new(agent, Some(execution.jobs().clone()), None)
        .unwrap()
        .with_execution(Some(execution.clone()));
    let tool = context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .get("run_tool")
        .unwrap();
    let cwd = root.path().to_path_buf();
    let handle = tokio::spawn(async move {
        tool.run(serde_json::json!({"tool":"bash", "arguments":{"command":"printf x >> count; printf 'out-ready\\n'; printf 'err-ready\\n' >&2; while [ ! -f release ]; do sleep 0.05; done; printf 'out-done\\n'"}}), &heycode_tools::ToolCtx::default().with_cwd(cwd)).await
    });
    let id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(id) = execution.foreground_jobs().first() {
                break id.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let key = TaskKey(format!("job:{id}"));
    assert!(
        source
            .snapshot()
            .unwrap()
            .iter()
            .find(|row| row.key == key)
            .unwrap()
            .capabilities
            .background
    );
    source
        .execute(
            TaskAction::Background(key.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["job_id"], id.as_str());
    assert_eq!(result["promoted"], true);
    // The foreground waiter has returned while the same process is still held.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if std::fs::read(root.path().join("count")).is_ok_and(|bytes| bytes == b"x") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(root.path().join("release"), "").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        execution.jobs().wait_for_settlement(&id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        std::fs::read(root.path().join("count")).unwrap(),
        b"x",
        "promotion must never restart the command"
    );
    assert!(
        !source
            .snapshot()
            .unwrap()
            .iter()
            .find(|row| row.key == key)
            .unwrap()
            .capabilities
            .background
    );
    // General run_tool owns one terminal result; ordinary background_shell has streaming lanes.
    let shell = execution
        .start_shell(
            "two streams",
            heycode_exec::ShellRequest::new("printf stdout-token; printf stderr-token >&2")
                .unwrap(),
            heycode_session::InboxDelivery::Inject,
        )
        .unwrap();
    execution.jobs().wait_for_settlement(&shell).await.unwrap();
    let key = TaskKey(format!("job:{shell}"));
    let stdout = source
        .output_channel(&key, None, 128, TaskOutputChannel::Stdout)
        .unwrap();
    let stderr = source
        .output_channel(&key, None, 128, TaskOutputChannel::Stderr)
        .unwrap();
    assert!(stdout.events.iter().any(|e| matches!(&e.kind, TaskOutputKind::Text(text) if text.contains("stdout-token") && !text.contains("stderr-token"))));
    assert!(stderr.events.iter().any(|e| matches!(&e.kind, TaskOutputKind::Text(text) if text.contains("stderr-token") && !text.contains("stdout-token"))));
    context.shutdown();
}

#[test]
fn task_console_rejected_message_returns_to_correct_child_without_overwriting_newer_draft() {
    let (mut state, _) = state();
    state.input.insert_str("parent safe");
    state.open_task(TaskKey("child:a".into()));
    state.input.insert_str("newer child draft");
    state.return_from_task();
    state.restore_failed_task_message(TaskKey("child:a".into()), "rejected message".into());
    assert_eq!(state.input.lines(), &["parent safe"]);
    state.open_task(TaskKey("child:a".into()));
    assert_eq!(
        state.input.lines(),
        &["rejected message", "", "newer child draft"]
    );
}

#[test]
fn task_console_toggle_respects_persisted_keymap_override() {
    let (mut state, _) = state();
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        heycode_ui::keymap::KeymapAction::ToggleTasks,
        heycode_ui::keymap::KeyChord::parse("ctrl+g").unwrap(),
    );
    state.set_keymap(heycode_ui::keymap::Keymap::resolve(&overrides).unwrap());
    press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert_eq!(state.task_console().view, ConsoleView::Collapsed);
    press(&mut state, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert_eq!(state.task_console().view, ConsoleView::List);
}

#[test]
fn task_console_narrow_details_wrap_controls_and_metadata_scrolls_from_top() {
    let (mut state, source) = state();
    source.rows.lock().unwrap()[0].telemetry.changed_paths =
        (0..35).map(|n| format!("changed/path-{n:02}.rs")).collect();
    state.refresh_tasks();
    state.open_task(TaskKey("child:a".into()));
    let rendered = text(&frame(&mut state, 40, 30));
    assert!(rendered.contains("@Child a"), "{rendered}");
    assert!(!rendered.contains("[Alt+I Stop]"), "{rendered}");
    press(&mut state, KeyCode::Char('m'), KeyModifiers::ALT);
    let top = text(&frame(&mut state, 40, 30));
    press(&mut state, KeyCode::PageDown, KeyModifiers::NONE);
    let later = text(&frame(&mut state, 40, 30));
    assert_ne!(top, later);
    assert!(later.contains("changed/path-"), "{later}");
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    assert_eq!(top, text(&frame(&mut state, 40, 30)));
}

#[tokio::test]
async fn task_console_team_dependency_and_claimed_mail_survive_committed_projection() {
    use heycode_session::{
        SessionEventKind, TeamChange, TeamId, TeamMailboxMessage, TeamMember, TeamMemberId,
        TeamRole, TeamTask, TeamTaskId,
    };
    let root = tempfile::tempdir().unwrap();
    let mut context = native_world(
        root.path(),
        Arc::new(HeldProvider {
            release: Arc::new(tokio::sync::Notify::new()),
            entered: std::sync::atomic::AtomicUsize::new(0),
        }),
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let source = RegistryTaskSource::new(agent.clone(), None, None).unwrap();
    assert!(source.snapshot().unwrap().is_empty());
    let team = TeamId::new("review-team").unwrap();
    let lead = TeamMember::new(TeamMemberId::new("lead").unwrap(), "Lead", TeamRole::Lead).unwrap();
    let worker = TeamMember::new(
        TeamMemberId::new("worker").unwrap(),
        "Worker",
        TeamRole::Worker,
    )
    .unwrap();
    let changes = [
        TeamChange::created(team.clone(), lead.clone()).unwrap(),
        TeamChange::member_added(&team, 1, lead.id(), worker.clone()).unwrap(),
        TeamChange::task_created(
            &team,
            2,
            lead.id(),
            TeamTask::new(
                TeamTaskId::new("build").unwrap(),
                "Build",
                worker.id().clone(),
                vec![],
            )
            .unwrap(),
        )
        .unwrap(),
        TeamChange::task_created(
            &team,
            3,
            lead.id(),
            TeamTask::new(
                TeamTaskId::new("review").unwrap(),
                "Review",
                lead.id().clone(),
                vec![TeamTaskId::new("build").unwrap()],
            )
            .unwrap(),
        )
        .unwrap(),
        TeamChange::message_sent(
            &team,
            4,
            lead.id(),
            TeamMailboxMessage::new(
                "mail-1",
                lead.id().clone(),
                worker.id().clone(),
                "Check the native result",
            )
            .unwrap(),
        )
        .unwrap(),
        TeamChange::message_claimed(&team, 5, worker.id(), "mail-1").unwrap(),
    ];
    for change in changes {
        agent
            .session()
            .lock()
            .unwrap()
            .append(SessionEventKind::TeamChange {
                change: Box::new(change),
            })
            .unwrap();
    }
    let key = TaskKey("team:review-team".into());
    let rows = source.snapshot().unwrap();
    assert_eq!(
        rows.iter().find(|r| r.key == key).unwrap().status,
        TaskStatus::Waiting
    );
    let page = source.output(&key, None, 128).unwrap();
    let output = format!("{:?}", page.events);
    assert!(
        output.contains("dependencies [build] · not ready"),
        "{output}"
    );
    assert!(
        output.contains("claimed true · Check the native result"),
        "{output}"
    );
    assert!(output.contains("Member worker"), "{output}");
    context.shutdown();
}

#[test]
fn task_console_approval_preempts_child_controls_without_consuming_either_draft() {
    let (mut state, _) = state();
    state.input.insert_str("parent draft");
    state.open_task(TaskKey("child:a".into()));
    state.input.insert_str("child draft");
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 71,
        name: "write".into(),
        args_preview: "path: guarded.txt".into(),
    });
    let rendered = text(&frame(&mut state, 100, 30));
    assert!(rendered.contains("Create file"), "{rendered}");
    press(&mut state, KeyCode::Char('i'), KeyModifiers::ALT);
    press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(
        state.pending_ask.is_some(),
        "approval focus must suppress task controls"
    );
    assert!(
        !state
            .task_console()
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("request")),
        "task controls must remain untouched"
    );
    state.apply(&heycode_agent::UiEvent::ApprovalResolved {
        id: 71,
        allowed: false,
    });
    state.return_from_task();
    assert_eq!(state.input.lines(), &["parent draft"]);
    state.open_task(TaskKey("child:a".into()));
    assert_eq!(state.input.lines(), &["child draft"]);
}

#[test]
fn parity_list_focus_keeps_the_active_child_and_its_composer_until_selection() {
    let (mut state, _) = state();
    state.open_task(TaskKey("child:a".into()));
    state.input.insert_str("alpha draft");
    state.open_task_category(TaskCategory::Agents);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        state.task_console().focused,
        Some(TaskKey("child:b".into()))
    );
    assert_eq!(
        state.task_console().selected,
        Some(TaskKey("child:a".into()))
    );
    assert_eq!(state.input.lines(), &["alpha draft"]);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        state.task_console().selected,
        Some(TaskKey("child:b".into()))
    );
    assert_eq!(state.input.lines(), &["alpha draft"]);
    assert!(state.task_console().preview);
    press(&mut state, KeyCode::Char('f'), KeyModifiers::NONE);
    assert_eq!(state.input.lines(), &[""]);
    state.open_task(TaskKey("child:a".into()));
    assert_eq!(state.input.lines(), &["alpha draft"]);
}

#[test]
fn parity_child_approval_names_its_owner_without_marking_a_parent_tool() {
    let (mut state, _) = state();
    state.apply(&heycode_agent::UiEvent::ToolStarted {
        name: "write".into(),
        args: serde_json::json!({"path":"parent.txt"}),
    });
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: Some("session-b".into()),
        id: 81,
        name: "write".into(),
        args_preview: "path: child.txt".into(),
    });
    let rendered = text(&frame(&mut state, 100, 30));
    assert!(rendered.contains("Child b"), "{rendered}");
    assert!(
        matches!(state.items.last(), Some(heycode_tui::app::Item::Tool {view, ..}) if view.approval.is_none())
    );
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.pending_ask.is_none());
    assert!(
        matches!(state.items.last(), Some(heycode_tui::app::Item::Tool {view, ..}) if view.approval.is_none())
    );
}

#[tokio::test]
async fn cancelled_root_preserves_queued_steer_until_explicit_resume() {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(HeldProvider {
        release: Arc::new(tokio::sync::Notify::new()),
        entered: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut context = native_world(root.path(), provider.clone());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let cancel = CancellationToken::new();
    let running = {
        let agent = agent.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { agent.send_cancellable("first turn", cancel).await })
    };
    async fn entered(provider: &HeldProvider, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while provider.entered.load(std::sync::atomic::Ordering::SeqCst) < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    entered(&provider, 1).await;
    let (id, wake) = agent
        .submit_inbox(heycode_session::InboxDelivery::Steer, "queued turn")
        .unwrap();
    assert_eq!(wake, heycode_agent::InboxWake::Queued);
    cancel.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(agent.next_wakeable_message(), Some(id.clone()));
    assert!(!agent.token().is_turn_active());
    assert_eq!(
        provider.entered.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let refused = agent
        .send_inbox_id_cancellable(&id, CancellationToken::new())
        .await;
    assert!(
        refused.is_err(),
        "pending dispatch must not resurrect a stopped root"
    );
    assert_eq!(agent.next_wakeable_message(), Some(id.clone()));
    let second_cancel = CancellationToken::new();
    let second = {
        let agent = agent.clone();
        let cancel = second_cancel.clone();
        tokio::spawn(async move { agent.send_cancellable("resume explicitly", cancel).await })
    };
    entered(&provider, 2).await;
    assert!(agent.pending_human_messages().is_empty());
    second_cancel.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap();
    assert!(!agent.token().is_turn_active());
    assert!(agent.next_wakeable_message().is_none());
    let session = agent.session().lock().unwrap();
    let users = session
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            heycode_session::SessionEventKind::UserMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(users, ["first turn", "resume explicitly", "queued turn"]);
    drop(session);
    context.shutdown();
}

#[test]
fn named_agent_completion_requires_attributed_notice_and_unique_job_owner() {
    use heycode_session::{
        InboxDelivery, InboxMessage, InboxMessageId, InboxSource, InboxTarget, SessionEvent,
        SessionEventKind,
    };
    let (mut state, source) = state();
    let notice = "[job job-0 completed] retained child output";
    let message = InboxMessage::with_source(
        InboxMessageId::generate(),
        InboxDelivery::FollowUp,
        notice,
        InboxSource::Job {
            job_id: "job-0".into(),
        },
    )
    .unwrap();
    state.replay(
        &[
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: Some(0),
                inserted: vec![message],
                outcome: None,
            },
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: Some(1),
                inserted: vec![],
                outcome: None,
            },
            SessionEventKind::UserMessage {
                text: notice.into(),
            },
            SessionEventKind::UserMessage {
                text: "[job job-0 completed] human-authored lookalike".into(),
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(seq, kind)| SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: seq as u64,
            time_ms: 0,
            kind,
        })
        .collect::<Vec<_>>(),
    );
    state.refresh_tasks();
    let replayed = text(&frame(&mut state, 100, 35));
    assert!(
        !replayed.contains("Agent \"Child a\" finished"),
        "{replayed}"
    );
    assert!(replayed.contains("retained child output"), "{replayed}");

    source.rows.lock().unwrap()[0].job = Some("job-0".into());
    source.rows.lock().unwrap()[0].status = TaskStatus::Completed;
    state.refresh_tasks();
    let named = text(&frame(&mut state, 100, 35));
    assert_eq!(named.matches("○ Child a completed").count(), 1, "{named}");
    assert!(!named.contains("retained child output"), "{named}");
    assert!(
        named.contains("❯ [job job-0 completed] human-authored lookalike"),
        "{named}"
    );

    // Once provenance is resolved, later registry changes cannot rewrite history.
    source.rows.lock().unwrap()[1].job = Some("job-0".into());
    source.rows.lock().unwrap()[1].status = TaskStatus::Completed;
    state.refresh_tasks();
    let ambiguous = text(&frame(&mut state, 100, 35));
    assert_eq!(
        ambiguous.matches("○ Child a completed").count(),
        1,
        "{ambiguous}"
    );
    assert!(!ambiguous.contains("retained child output"), "{ambiguous}");
}

#[test]
fn single_agent_card_preserves_lifecycle_and_receipt_navigation() {
    for status in [
        TaskStatus::Running,
        TaskStatus::Waiting,
        TaskStatus::Idle,
        TaskStatus::Completed,
        TaskStatus::Failed,
    ] {
        let (mut state, source) = state();
        {
            let mut rows = source.rows.lock().unwrap();
            rows.truncate(1);
            rows[0].status = status;
            rows[0].telemetry.spawn_call_id = Some("single-spawn".into());
        }
        state.replay(&[heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 0,
            time_ms: 0,
            kind: heycode_session::SessionEventKind::ToolCall {
                turn: 1,
                call_id: heycode_core::CallId::from_raw("single-spawn"),
                name: "agent".into(),
                args: serde_json::json!({"label":"Child a", "provider":"native"}),
            },
        }]);
        state.refresh_tasks();
        for (width, height) in [(100, 35), (45, 25), (20, 25)] {
            let buffer = frame(&mut state, width, height);
            let rendered = text(&buffer);
            assert!(rendered.contains("Agent(Child a)"), "{rendered}");
            if width >= 45 {
                assert!(
                    rendered.contains(&format!("Backgrounded agent · {}", status.label())),
                    "{rendered}"
                );
            }
            if width < 100 {
                assert!(!rendered.contains("header expands"), "{rendered}");
            }
            assert!(!rendered.contains("● 1 agent"), "{rendered}");
            let receipt_row = buffer
                .content
                .chunks(usize::from(width))
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .position(|line| line.contains('⎿') && line.contains(status.label()))
                .unwrap();
            let hit = state
                .task_console()
                .hits
                .iter()
                .find(|(area, hit)| {
                    usize::from(area.y) == receipt_row
                        && *hit == TaskHit::Open(TaskKey("child:a".into()))
                })
                .unwrap()
                .0;
            state.handle_terminal_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x,
                row: hit.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert_eq!(
                state.task_console().selected,
                Some(TaskKey("child:a".into()))
            );
            state.return_from_task();
            press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
        }
    }
}

#[test]
fn parity_spawn_tree_correlates_exact_call_and_child_click_not_duplicate_label() {
    let (mut state, source) = state();
    let theme = heycode_ui::theme::builtin_themes()
        .unwrap()
        .into_iter()
        .find(|theme| theme.id().as_str() == "heycode-light")
        .unwrap();
    let capabilities = heycode_ui::terminal::TerminalCapabilities::detect(
        &heycode_ui::terminal::TerminalEnvironment::new()
            .with_term(Some("xterm-256color"))
            .with_colorterm(Some("truecolor")),
    );
    state.apply_terminal(capabilities, &theme);
    for (index, row) in source.rows.lock().unwrap().iter_mut().enumerate() {
        row.label = "Same worker label".into();
        row.telemetry.spawn_call_id = Some(
            if index < 2 {
                "spawn-pair"
            } else {
                "other-call"
            }
            .into(),
        );
    }
    state.replay(&[
        heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 0,
            time_ms: 0,
            kind: heycode_session::SessionEventKind::ToolCall {
                turn: 1,
                call_id: heycode_core::CallId::from_raw("spawn-pair"),
                name: "execute".into(),
                args: serde_json::json!({"code":"spawn two workers"}),
            },
        },
        heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 1,
            time_ms: 0,
            kind: heycode_session::SessionEventKind::ToolCall {
                turn: 1,
                call_id: heycode_core::CallId::from_raw("other-call"),
                name: "task".into(),
                args: serde_json::json!({"label":"Same worker label"}),
            },
        },
    ]);
    state.refresh_tasks();
    for (width, height) in [(100, 35), (45, 25)] {
        let buffer = frame(&mut state, width, height);
        let light_colors = heycode_ui::theme::HEYCODE_LIGHT
            .map(|rgb| ratatui::style::Color::Rgb(rgb.r, rgb.g, rgb.b));
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.fg == ratatui::style::Color::Reset
                    || light_colors.contains(&cell.fg)),
            "light-theme agent tree must use its resolved palette, including shared gray border values"
        );
        let rendered = text(&buffer);
        assert!(rendered.contains("3 agents"), "{rendered}");
        assert!(
            !rendered.contains("session-a"),
            "internal IDs are not UI labels"
        );
        let target = state
            .task_console()
            .hits
            .iter()
            .find(|(_, hit)| *hit == TaskHit::Open(TaskKey("child:b".into())))
            .unwrap()
            .0;
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.x,
            row: target.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(
            state.task_console().selected,
            Some(TaskKey("child:b".into()))
        );
        state.return_from_task();
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    }
}

#[test]
fn parity_idle_child_hides_resolved_approval_diagnostics_and_parent_activity() {
    let (mut state, source) = state();
    source.rows.lock().unwrap()[0].status = TaskStatus::Idle;
    state.refresh_tasks();
    *source.page.lock().unwrap() = Some(TaskOutputPage {
        events: vec![
            TaskOutputEvent {
                sequence: 1,
                kind: TaskOutputKind::Status("Waiting for approval 73: read".into()),
            },
            TaskOutputEvent {
                sequence: 2,
                kind: TaskOutputKind::Status("Approval 73: allowed".into()),
            },
            TaskOutputEvent {
                sequence: 3,
                kind: TaskOutputKind::Text("Read the `multiword code phrase`.".into()),
            },
        ],
        ..TaskOutputPage::default()
    });
    state.open_task(TaskKey("child:a".into()));
    let rendered = text(&frame(&mut state, 100, 30));
    assert!(rendered.contains("multiword code phrase"));
    assert!(!rendered.contains("approval 73"));
    assert!(!rendered.contains("Approval 73"));
    assert!(!rendered.contains(state.spinner_glyph()));
}

#[test]
fn bottom_categories_never_mix_agents_with_jobs_and_preserve_parent_draft() {
    let (mut state, source) = state();
    let mut job = row("process", TaskStatus::Running);
    job.key = TaskKey("job:process".into());
    job.kind = TaskKind::Job;
    source.rows.lock().unwrap().push(job);
    state.refresh_tasks();
    state.input.insert_str("preserve my draft");
    for (width, height) in [(80, 24), (100, 45), (160, 45)] {
        let display = text(&frame(&mut state, width, height));
        assert!(
            display.contains("2 agents") && !display.contains("Jobs 1"),
            "{display}"
        );
        assert!(display.contains("1 shell"), "{display}");
    }
    state.open_task_category(TaskCategory::Agents);
    assert_eq!(state.task_console().visible_records().len(), 3);
    assert!(
        state
            .task_console()
            .visible_records()
            .iter()
            .all(|r| r.kind == TaskKind::Child)
    );
    press(&mut state, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(state.task_console().category, TaskCategory::Work);
    assert!(state.task_console().visible_records().is_empty());
    press(&mut state, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(state.task_console().category, TaskCategory::Jobs);
    assert_eq!(state.task_console().visible_records().len(), 1);
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        state.task_console().selected.as_ref().unwrap().0,
        "job:process"
    );
    state.return_from_task();
    assert_eq!(state.input.lines().join("\n"), "preserve my draft");
}

#[tokio::test]
async fn task_console_work_uses_durable_truth_and_never_exposes_process_controls() {
    use heycode_session::{WorkItemFields, WorkScope, WorkStatus};
    let root = tempfile::tempdir().unwrap();
    let mut context = native_world(
        root.path(),
        Arc::new(HeldProvider {
            release: Arc::new(tokio::sync::Notify::new()),
            entered: std::sync::atomic::AtomicUsize::new(0),
        }),
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let service = heycode_agent::WorkService::new(agent.session().clone());
    let source = Arc::new(RegistryTaskSource::new(agent.clone(), None, None).unwrap());
    let prerequisite = service
        .create(
            WorkScope::Session,
            "ui-prerequisite",
            WorkItemFields {
                subject: "Prepare durable fixture".into(),
                description: "Retain this prerequisite identity".into(),
                status: WorkStatus::Pending,
                owner: None,
                dependencies: vec![],
                metadata: Default::default(),
            },
        )
        .unwrap();
    let item = service
        .create(
            WorkScope::Session,
            "ui-work",
            WorkItemFields {
                subject: "Check Unicode café 東京".into(),
                description: "Verify retained detail".into(),
                status: WorkStatus::Pending,
                owner: Some("reviewer".into()),
                dependencies: vec![prerequisite.id().clone()],
                metadata: std::iter::once(("priority".into(), serde_json::json!("high"))).collect(),
            },
        )
        .unwrap();
    let rows = source.snapshot().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.kind == TaskKind::Work));
    let target_key = TaskKey(format!("work:{}", item.id().as_str()));
    let target = rows.iter().find(|row| row.key == target_key).unwrap();
    assert!(
        !target.capabilities.interrupt
            && !target.capabilities.steer
            && !target.capabilities.terminal_input
    );
    let (mut state, _) = state();
    state.set_task_source(source.clone());
    state.input.insert_str("Parent draft survives work view");
    state.open_task_category(TaskCategory::Work);
    for (width, height) in [(80, 24), (100, 45), (160, 45)] {
        let display = text(&frame(&mut state, width, height));
        assert!(display.contains("Work (2)"), "{display}");
        assert!(
            display.contains("Check Unicode café")
                && display.contains("東")
                && display.contains("京"),
            "{display}"
        );
        assert!(!display.contains("1 shell"), "{display}");
    }
    let target_index = state
        .task_console()
        .visible_records()
        .iter()
        .position(|row| row.key == target_key)
        .unwrap();
    for _ in 0..target_index {
        press(&mut state, KeyCode::Down, KeyModifiers::NONE);
    }
    press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.task_console().preview);
    assert!(!state.task_console().active);
    let detail = text(&frame(&mut state, 100, 45));
    assert!(detail.contains("Work · Check Unicode café"), "{detail}");
    assert!(detail.contains(item.id().as_str()), "{detail}");
    assert!(detail.contains(prerequisite.id().as_str()), "{detail}");
    assert!(detail.contains("State: pending"), "{detail}");
    assert!(detail.contains("Revision: 1"), "{detail}");
    assert!(detail.contains("Owner: reviewer"), "{detail}");
    assert!(detail.contains("Verify retained detail"), "{detail}");
    assert!(detail.contains(r#"{"priority":"high"}"#), "{detail}");
    for forbidden in [
        "@Check Unicode",
        "Elapsed unavailable",
        "Prompt unavailable",
        "No tools reported",
        "Message @",
        "to foreground",
        "to stop",
    ] {
        assert!(
            !detail.contains(forbidden),
            "unexpected {forbidden}: {detail}"
        );
    }
    let accessible = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(accessible.contains(item.id().as_str()), "{accessible}");
    assert!(
        accessible.contains(prerequisite.id().as_str()),
        "{accessible}"
    );
    assert!(accessible.contains("State: pending"), "{accessible}");
    assert!(!accessible.contains("elapsed milliseconds"), "{accessible}");
    assert!(!accessible.contains("actions: message"), "{accessible}");
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    state.open_task_category(TaskCategory::Work);
    frame(&mut state, 100, 45);
    let target_area = state
        .task_console()
        .hits
        .iter()
        .find(|(_, hit)| *hit == TaskHit::Open(target_key.clone()))
        .unwrap()
        .0;
    state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: target_area.x,
        row: target_area.y,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(state.task_console().preview);
    assert!(!state.task_console().active);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        state.input.lines().join("\n"),
        "Parent draft survives work view"
    );
    let mut fields = item.fields().clone();
    fields.status = WorkStatus::Blocked;
    service.update(item.id(), 1, fields.clone()).unwrap();
    assert_eq!(
        source
            .snapshot()
            .unwrap()
            .iter()
            .find(|row| row.key == target_key)
            .unwrap()
            .status_label(),
        "blocked"
    );
    let mut prerequisite_fields = prerequisite.fields().clone();
    prerequisite_fields.status = WorkStatus::Completed;
    service
        .update(prerequisite.id(), 1, prerequisite_fields)
        .unwrap();
    fields.status = WorkStatus::InProgress;
    service.update(item.id(), 2, fields.clone()).unwrap();
    state.refresh_tasks();
    let summary = state.task_console().summary();
    assert!(summary.contains("1 in progress"), "{summary}");
    assert!(!summary.contains("running"), "{summary}");
    fields.status = WorkStatus::Completed;
    service.update(item.id(), 3, fields.clone()).unwrap();
    assert_eq!(
        source
            .snapshot()
            .unwrap()
            .iter()
            .find(|row| row.key == target_key)
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    fields.status = WorkStatus::Deleted;
    service.update(item.id(), 4, fields).unwrap();
    assert!(
        source
            .snapshot()
            .unwrap()
            .iter()
            .all(|row| row.key != target_key)
    );
    context.shutdown();
}

#[test]
fn banner_precedes_transcript_and_agent_selector_follows_composer_at_one_five_and_twenty_agents() {
    for count in [1, 5, 20] {
        for (width, height) in [(80, 24), (100, 45), (160, 45)] {
            let (mut state, source) = state();
            *source.rows.lock().unwrap() = (0..count)
                .map(|index| {
                    let status = [
                        TaskStatus::Running,
                        TaskStatus::Waiting,
                        TaskStatus::Completed,
                        TaskStatus::Failed,
                    ][index % 4];
                    let mut row = row(&format!("scale-{index:02}"), status);
                    row.label = format!("Worker {index:02} café 東京 with a long descriptive name");
                    row
                })
                .collect();
            state.items.push(heycode_tui::app::Item::Assistant(
                "TRANSCRIPT_BEFORE_INPUT".into(),
            ));
            state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
            state.input.insert_str("UNSENT_COMPOSER_DRAFT");
            state.open_task_category(TaskCategory::Agents);
            for _ in 1..count {
                press(&mut state, KeyCode::Down, KeyModifiers::NONE);
            }
            let buffer = frame(&mut state, width, height);
            let lines: Vec<String> = (0..height)
                .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
                .collect();
            let locate = |value: &str| {
                lines
                    .iter()
                    .position(|line| line.contains(value))
                    .unwrap_or_else(|| panic!("missing {value}: {}", lines.join("\n")))
            };
            assert!(
                !lines
                    .iter()
                    .any(|line| line.contains("UNSENT_COMPOSER_DRAFT"))
            );
            assert!(locate("TRANSCRIPT_BEFORE_INPUT") < locate("Background"));
            assert!(locate("HeyCode 0.1.0") < locate("TRANSCRIPT_BEFORE_INPUT"));
            assert!(locate(&format!("Worker {:02}", count - 1)) > locate("Background"));
            let target = TaskKey(format!("child:scale-{:02}", count - 1));
            let hit = state
                .task_console()
                .hits
                .iter()
                .find(|(_, hit)| *hit == TaskHit::Open(target.clone()))
                .unwrap()
                .0;
            state.handle_terminal_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x,
                row: hit.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert_eq!(state.task_console().selected.as_ref(), Some(&target));
            state.return_from_task();
            assert_eq!(state.input.lines().join("\n"), "UNSENT_COMPOSER_DRAFT");
        }
    }
}

#[test]
fn permissions_and_shells_share_a_clickable_row_before_optional_agents_and_teams() {
    let (mut state, source) = state();
    let mut shell = row("shell", TaskStatus::Running);
    shell.kind = TaskKind::Job;
    shell.telemetry.command = Some("sleep 5".into());
    let mut team = row("team", TaskStatus::Waiting);
    team.kind = TaskKind::Team;
    *source.rows.lock().unwrap() = vec![row("agent", TaskStatus::Running), shell.clone(), team];
    state.refresh_tasks();
    state.permission = "full_access".into();
    state.reasoning_effort = Some("high".into());
    state.input.insert_str("FOOTER_ORDER_DRAFT");
    for (width, height) in [(80, 24), (100, 45), (160, 45)] {
        let buffer = frame(&mut state, width, height);
        let lines: Vec<String> = (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        let locate = |value: &str| {
            lines
                .iter()
                .position(|line| line.contains(value))
                .unwrap_or_else(|| panic!("missing {value}: {}", lines.join("\n")))
        };
        let input = locate("FOOTER_ORDER_DRAFT");
        assert!(
            !lines.iter().any(|line| line.contains("Working")),
            "idle effort does not create activity"
        );
        // One footer line under the composer: the permission mode first, then
        // the clickable shell, agent and team counts on the same row.
        let permissions = locate("⏵⏵ full access on");
        let agents = locate("1 agent");
        assert!(input < permissions && permissions == agents);
        assert!(!lines.iter().any(|line| line.contains("in:0 out:0")));
        assert_eq!(permissions, locate("1 shell"));
        assert_eq!(agents, locate("1 team"));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("Workflow") || line.contains("Work 0"))
        );
        let hit = state
            .task_console()
            .hits
            .iter()
            .find(|(_, hit)| *hit == TaskHit::Category(TaskCategory::All))
            .unwrap()
            .0;
        assert_eq!(usize::from(hit.y), permissions);
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(state.task_console().category, TaskCategory::All);
        assert_eq!(state.task_console().view, ConsoleView::List);
        assert_eq!(state.task_console().visible_records().len(), 3);
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    }
    *source.rows.lock().unwrap() = vec![shell];
    state.refresh_tasks();
    let only_shell = text(&frame(&mut state, 100, 45));
    assert!(only_shell.contains("1 shell"));
    assert!(
        !only_shell.contains("Agents")
            && !only_shell.contains("Teams")
            && !only_shell.contains("Ctrl+T browse")
    );
}

#[test]
fn shell_browser_only_displays_selected_output_and_back_preserves_running_shells() {
    struct Shells;
    #[async_trait]
    impl TaskSource for Shells {
        fn snapshot(&self) -> Result<Vec<TaskRecord>, String> {
            Ok(["one", "two"]
                .into_iter()
                .map(|id| {
                    let mut item = row(id, TaskStatus::Running);
                    item.kind = TaskKind::Job;
                    item.label = format!("Shell {id}");
                    item.telemetry.command = Some(format!("command-{id}"));
                    item.capabilities.steer = false;
                    item
                })
                .collect())
        }
        fn output(
            &self,
            key: &TaskKey,
            _: Option<u64>,
            _: usize,
        ) -> Result<TaskOutputPage, String> {
            Ok(TaskOutputPage {
                events: vec![TaskOutputEvent {
                    sequence: 1,
                    kind: TaskOutputKind::Text(format!("SELECTED_OUTPUT_{}", key.0)),
                }],
                ..Default::default()
            })
        }
        async fn execute(&self, _: TaskAction, _: CancellationToken) -> Result<String, String> {
            panic!("navigation must never stop or close a shell")
        }
    }
    for (width, height) in [(80, 24), (100, 45), (160, 45)] {
        let mut state = AppState::new("native-model", "/workspace".into());
        state.set_task_source(Arc::new(Shells));
        state.items.push(heycode_tui::app::Item::Assistant(
            "PARENT_TRANSCRIPT".into(),
        ));
        state.input.insert_str("parent draft");
        press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
        let list = text(&frame(&mut state, width, height));
        assert!(
            list.contains("Shell one") && list.contains("Shell two"),
            "{list}"
        );
        assert!(!list.contains("SELECTED_OUTPUT") && list.contains("PARENT_TRANSCRIPT"));
        for id in ["one", "two"] {
            let target = TaskKey(format!("child:{id}"));
            let rect = state
                .task_console()
                .hits
                .iter()
                .find(|(_, hit)| *hit == TaskHit::Open(target.clone()))
                .unwrap()
                .0;
            state.handle_terminal_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
            let detail = text(&frame(&mut state, width, height));
            assert!(
                detail.contains(&format!("SELECTED_OUTPUT_child:{id}")),
                "{detail}"
            );
            let other = if id == "one" { "two" } else { "one" };
            assert!(
                !detail.contains(&format!("SELECTED_OUTPUT_child:{other}"))
                    && detail.contains("PARENT_TRANSCRIPT")
            );
            press(&mut state, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(state.task_console().view, ConsoleView::List);
            assert_eq!(state.task_console().category, TaskCategory::All);
            assert_eq!(state.input.lines(), &["parent draft"]);
            let list = text(&frame(&mut state, width, height));
            assert!(
                list.contains("Shell one")
                    && list.contains("Shell two")
                    && !list.contains("SELECTED_OUTPUT")
            );
        }
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
        assert!(text(&frame(&mut state, width, height)).contains("PARENT_TRANSCRIPT"));
    }
}

#[test]
fn background_inspector_preserves_foreground_owner_and_never_edits_hidden_draft() {
    let (mut state, _) = state();
    state.input.insert_str("parent draft");
    state.open_task(TaskKey("child:a".into()));
    state.input.insert_str("draft owned by a");
    press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
    frame(&mut state, 80, 24);
    let hit = state
        .task_console()
        .hits
        .iter()
        .find(|(_, hit)| *hit == TaskHit::Open(TaskKey("child:b".into())))
        .unwrap()
        .0;
    state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.x,
        row: hit.y,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(state.task_console().preview);
    press(&mut state, KeyCode::Char('z'), KeyModifiers::NONE);
    state.handle_terminal_event(&Event::Paste("must not enter either draft".into()));
    assert_eq!(state.input.lines(), &["draft owned by a"]);
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!state.task_console().preview);
    assert!(state.task_console().active);
    assert_eq!(
        state.task_console().selected,
        Some(TaskKey("child:a".into()))
    );
    assert_eq!(state.input.lines(), &["draft owned by a"]);
    assert_eq!(
        state.task_console().view,
        ConsoleView::List,
        "Esc restores the selector under the preview"
    );
    frame(&mut state, 40, 30);
    let hit = state
        .task_console()
        .hits
        .iter()
        .find(|(_, hit)| *hit == TaskHit::Open(TaskKey("child:b".into())))
        .unwrap()
        .0;
    state.handle_terminal_event(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.x,
        row: hit.y,
        modifiers: KeyModifiers::NONE,
    }));
    let preview = text(&frame(&mut state, 40, 30));
    assert!(
        preview.contains("f to foreground") && preview.contains("x to stop"),
        "{preview}"
    );
    press(&mut state, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(!state.task_console().preview);
    assert_eq!(
        state.task_console().selected,
        Some(TaskKey("child:b".into()))
    );
    assert_eq!(state.input.lines(), &[""]);
    state.open_task(TaskKey("child:a".into()));
    assert_eq!(state.input.lines(), &["draft owned by a"]);
    press(&mut state, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(state.input.lines(), &["parent draft"]);
}

#[test]
fn agent_inspector_matches_progress_and_prompt_structure_with_failed_tool_detail() {
    for (width, height) in [(100, 40), (45, 30)] {
        let (mut state, source) = state();
        source.rows.lock().unwrap()[0].status = TaskStatus::Failed;
        source.rows.lock().unwrap()[0].telemetry.initial_prompt =
            Some("OLD_PROMPT_ONLY".repeat(50));
        *source.page.lock().unwrap() = Some(TaskOutputPage {
            events: vec![
                TaskOutputEvent {
                    sequence: 1,
                    kind: TaskOutputKind::User("OLD_PROMPT_ONLY".repeat(50)),
                },
                TaskOutputEvent {
                    sequence: 2,
                    kind: TaskOutputKind::ToolMetadata {
                        call_id: "read-1".into(),
                        name: "read".into(),
                        args: serde_json::json!({"path": "missing.rs"}),
                    },
                },
                TaskOutputEvent {
                    sequence: 3,
                    kind: TaskOutputKind::ToolStarted {
                        call_id: "read-1".into(),
                        name: "read".into(),
                    },
                },
                TaskOutputEvent {
                    sequence: 4,
                    kind: TaskOutputKind::ToolFinished {
                        call_id: "read-1".into(),
                        ok: false,
                        text: "FILE_NOT_FOUND: missing.rs".into(),
                    },
                },
                TaskOutputEvent {
                    sequence: 5,
                    kind: TaskOutputKind::Status("Agent stopped after read failure".into()),
                },
            ],
            ..Default::default()
        });
        press(&mut state, KeyCode::Char('t'), KeyModifiers::CONTROL);
        frame(&mut state, width, height);
        let rect = state
            .task_console()
            .hits
            .iter()
            .find(|(_, hit)| *hit == TaskHit::Open(TaskKey("child:a".into())))
            .unwrap()
            .0;
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        }));
        let rendered = text(&frame(&mut state, width, height));
        let compact = rendered
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(rendered.contains("Progress"), "{rendered}");
        assert!(rendered.contains("Prompt"), "{rendered}");
        assert!(compact.contains("FILE_NOT_FOUND"), "{rendered}");
        assert!(compact.contains("Read(missing.rs)·failed"), "{rendered}");
        assert!(rendered.contains("f to foreground"), "{rendered}");
    }
}

#[test]
fn direct_agent_switcher_scales_and_keyboard_mouse_navigation_preserve_drafts_and_cursors() {
    for count in [1, 5, 20] {
        for (width, height) in [(80, 24), (100, 45), (160, 45)] {
            let (mut state, source) = state();
            *source.rows.lock().unwrap() = (0..count)
                .map(|index| {
                    let mut item = row(
                        &format!("direct-{index:02}"),
                        [
                            TaskStatus::Running,
                            TaskStatus::Waiting,
                            TaskStatus::Completed,
                            TaskStatus::Failed,
                        ][index % 4],
                    );
                    item.label = format!("Agent {index:02} café 東京 with retained context");
                    item.telemetry.initial_prompt =
                        Some(format!("Inspect fixture {index:02} without mutation"));
                    item
                })
                .collect();
            state.refresh_tasks();

            let parent_draft = "parent line one\nparent line two";
            state.input.insert_str(parent_draft);
            state.input.move_cursor(tui_textarea::CursorMove::Head);
            let parent_cursor = state.input.cursor();
            let initial = text(&frame(&mut state, width, height));
            assert!(initial.contains("● main"), "{width}x{height}: {initial}");
            assert!(
                !initial.contains("↓ to manage"),
                "the right column is elapsed time: {initial}"
            );
            assert!(initial.contains("Agent 00"), "{width}x{height}: {initial}");

            // Empty composer is the explicit keyboard handoff into the direct
            // footer; the parent draft and cursor are restored after return.
            state.input = tui_textarea::TextArea::default();
            press(&mut state, KeyCode::Down, KeyModifiers::NONE);
            let main_focused = text(&frame(&mut state, width, height));
            assert!(main_focused.contains("● main"), "{main_focused}");
            for _ in 0..count {
                press(&mut state, KeyCode::Down, KeyModifiers::NONE);
            }
            let target_index = (0..count).rev().find(|index| index % 4 < 2).unwrap();
            let target = TaskKey(format!("child:direct-{target_index:02}"));
            assert_eq!(state.task_console().focused.as_ref(), Some(&target));
            let focused = text(&frame(&mut state, width, height));
            assert!(
                focused.contains(&format!("Agent {target_index:02}")),
                "{width}x{height}: {focused}"
            );
            assert!(
                focused.contains("● main"),
                "keyboard focus alone must not change the selected conversation: {focused}"
            );
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(state.task_console().selected.as_ref(), Some(&target));
            assert!(state.task_console().active);
            state.input.insert_str("child draft tail");
            state.input.move_cursor(tui_textarea::CursorMove::Head);
            let child_cursor = state.input.cursor();

            let child_view = text(&frame(&mut state, width, height));
            assert!(
                child_view.contains("○ main"),
                "unselected main must be hollow: {child_view}"
            );
            assert!(
                child_view.contains(&format!("● Agent {target_index:02}")),
                "only the selected agent is filled: {child_view}"
            );
            assert!(
                child_view.contains(&format!("Agent {target_index:02}")),
                "{width}x{height}: {child_view}"
            );
            let first_hit = state
                .task_console()
                .hits
                .iter()
                .find(|(_, hit)| *hit == TaskHit::Switch(TaskKey("child:direct-00".into())))
                .map(|(area, _)| *area);
            if let Some(first_hit) = first_hit {
                state.handle_terminal_event(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: first_hit.x,
                    row: first_hit.y,
                    modifiers: KeyModifiers::NONE,
                }));
                assert_eq!(
                    state.task_console().selected,
                    Some(TaskKey("child:direct-00".into()))
                );
                state.open_task(target.clone());
                assert_eq!(state.input.lines(), &["child draft tail"]);
                assert_eq!(state.input.cursor(), child_cursor);
            }

            // Install the preserved parent draft in its owner slot, then prove
            // both the content and cursor return with the main conversation.
            state.return_from_task();
            state.input.insert_str(parent_draft);
            state.input.move_cursor(tui_textarea::CursorMove::Head);
            assert_eq!(state.input.cursor(), parent_cursor);
            state.open_task(target);
            assert_eq!(state.input.lines(), &["child draft tail"]);
            assert_eq!(state.input.cursor(), child_cursor);
            state.return_from_task();
            assert_eq!(state.input.lines(), &["parent line one", "parent line two"]);
            assert_eq!(state.input.cursor(), parent_cursor);
        }
    }
}

#[test]
fn task_console_foreground_footer_uses_child_totals_and_restores_current_parent_usage() {
    for width in [100, 160] {
        let (mut state, source) = state();
        {
            let mut rows = source.rows.lock().unwrap();
            rows[0].telemetry.model = Some("child-model-a".into());
            rows[0].telemetry.input_tokens = Some(21_021);
            rows[0].telemetry.output_tokens = Some(211);
            rows[1].telemetry.model = Some("child-model-b".into());
            rows[1].telemetry.input_tokens = Some(32_032);
            rows[1].telemetry.output_tokens = Some(321);
        }
        state.refresh_tasks();
        state.input.insert_str("parent draft");
        state.usage = Some(heycode_core::TokenUsage {
            prompt_tokens: 9_001,
            completion_tokens: 901,
        });
        state.context_tokens = Some(8_000);
        state.context_window = Some(100_000);
        state.open_task(TaskKey("child:a".into()));
        let child_a = text(&frame(&mut state, width, 35));
        assert!(child_a.contains("child-model-a"), "{child_a}");
        assert!(child_a.contains("total in:21k out:211"), "{child_a}");
        assert!(child_a.contains("context unavailable"), "{child_a}");
        assert!(!child_a.contains("in:9k out:901"), "{child_a}");
        assert!(!child_a.contains("ctx:~8k/100k"), "{child_a}");

        // Parent events keep updating the parent while a different owner is visible.
        state.apply(&heycode_agent::UiEvent::TurnFinished {
            reason: "stop".into(),
            usage: Some(heycode_core::TokenUsage {
                prompt_tokens: 34_567,
                completion_tokens: 891,
            }),
            context_tokens: Some(6_000),
        });
        state.open_task(TaskKey("child:b".into()));
        let child_b = text(&frame(&mut state, width, 35));
        assert!(child_b.contains("child-model-b"), "{child_b}");
        assert!(child_b.contains("total in:32k out:321"), "{child_b}");
        assert!(!child_b.contains("total in:21k out:211"), "{child_b}");
        assert!(!child_b.contains("in:34.5k out:891"), "{child_b}");
        assert_eq!(state.usage.unwrap().prompt_tokens, 34_567);
        assert_eq!(state.context_tokens, Some(6_000));

        state.return_from_task();
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
        assert!(state.foreground_task().is_none());
        assert_eq!(state.input.lines(), &["parent draft"]);
        let parent = text(&frame(&mut state, width, 35));
        // Returning restores the parent's own context and measured usage.
        assert!(!parent.contains("total in:"), "{parent}");
        assert!(parent.contains("ctx:~6k/100k"), "{parent}");
        let flat =
            heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
        assert!(flat.contains("tokens: in 34567 out 891"), "{flat}");
        assert!(flat.contains("context: ~6000"), "{flat}");

        state.open_tasks();
        let background = text(&frame(&mut state, width, 35));
        assert!(background.contains("@main"), "{background}");
        assert!(!background.contains("@team-lead"), "{background}");
    }
}

#[test]
fn task_console_foreground_owner_survives_inspection_reordering_and_missing_records() {
    fn inspect(state: &mut AppState, key: &str) {
        state.open_tasks();
        frame(state, 100, 40);
        let target = TaskHit::Open(TaskKey(key.into()));
        let area = state
            .task_console()
            .hits
            .iter()
            .find(|(_, hit)| *hit == target)
            .unwrap()
            .0;
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(state.task_console().preview);
    }

    let (mut state, source) = state();
    state.input.insert_str("main draft");
    inspect(&mut state, "child:b");
    assert!(
        state.foreground_task().is_none(),
        "inspecting is not foregrounding"
    );
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    state.open_task(TaskKey("child:a".into()));
    state.input.insert_str("A draft");
    inspect(&mut state, "child:b");
    assert_eq!(
        state.task_console().selected_record().unwrap().label,
        "Child b"
    );
    let owner = state.foreground_task().unwrap();
    assert_eq!(owner.key.unwrap().0, "child:a");
    assert_eq!(owner.record.unwrap().label, "Child a");
    assert_eq!(state.input.lines(), &["A draft"]);

    {
        let mut rows = source.rows.lock().unwrap();
        rows.reverse();
        let a = rows.iter_mut().find(|row| row.key.0 == "child:a").unwrap();
        a.telemetry.input_tokens = Some(70_001);
        a.telemetry.output_tokens = Some(707);
    }
    state.refresh_tasks();
    let owner = state.foreground_task().unwrap();
    assert_eq!(owner.key.unwrap().0, "child:a");
    assert_eq!(owner.record.unwrap().telemetry.input_tokens, Some(70_001));
    source
        .rows
        .lock()
        .unwrap()
        .retain(|row| row.key.0 != "child:a");
    state.refresh_tasks();
    let unavailable = state.foreground_task().unwrap();
    assert_eq!(unavailable.key.unwrap().0, "child:a");
    assert!(
        unavailable.record.is_none(),
        "neither parent nor inspected B may substitute"
    );
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.foreground_task().unwrap().record.is_none());
    state.return_from_task();
    assert!(state.foreground_task().is_none());
    assert_eq!(state.input.lines(), &["main draft"]);
}

#[test]
fn task_console_foreground_footer_never_borrows_parent_data_when_child_data_is_unknown() {
    let (mut state, source) = state();
    source.rows.lock().unwrap()[0].telemetry = TaskTelemetry::default();
    state.refresh_tasks();
    state.usage = Some(heycode_core::TokenUsage {
        prompt_tokens: 9_001,
        completion_tokens: 901,
    });
    state.context_tokens = Some(8_000);
    state.context_window = Some(100_000);
    state.open_task(TaskKey("child:a".into()));
    for removed in [false, true] {
        if removed {
            source
                .rows
                .lock()
                .unwrap()
                .retain(|row| row.key.0 != "child:a");
            state.refresh_tasks();
        }
        let child = text(&frame(&mut state, 100, 35));
        assert!(child.contains("model unavailable"), "{child}");
        assert!(child.contains("context unavailable"), "{child}");
        assert!(child.contains("total in:— out:—"), "{child}");
        assert!(!child.contains("in:9k out:901"), "{child}");
        assert!(!child.contains("ctx:~8k/100k"), "{child}");
    }
}

#[test]
fn active_agent_projection_settles_without_losing_history_or_selected_inspector() {
    let (mut state, source) = state();
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.open_task(TaskKey("child:a".into()));
    for child in source.rows.lock().unwrap().iter_mut() {
        child.status = TaskStatus::Completed;
        child.telemetry.current_tool = None;
    }
    source.rows.lock().unwrap()[1].status = TaskStatus::Idle;
    state.refresh_tasks();
    assert!(
        state.task_console().active,
        "selected settled inspector stays open"
    );
    assert_eq!(
        state.task_console().selected_record().unwrap().status,
        TaskStatus::Completed
    );
    state.return_from_task();
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    state.apply(&heycode_agent::UiEvent::AssistantDelta {
        text: "Here are the combined findings.".into(),
    });
    let responding = text(&frame(&mut state, 110, 42));
    assert!(responding.contains("Responding"), "{responding}");
    assert!(
        state.has_active_turn(),
        "child settlement cannot stop the parent"
    );
    assert!(
        !state
            .task_console()
            .hits
            .iter()
            .any(|(_, hit)| matches!(hit, TaskHit::Switch(_)))
    );
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "completed".into(),
        usage: None,
        context_tokens: None,
    });
    let settled = text(&frame(&mut state, 60, 24));
    assert!(
        !settled.contains("Working") && !settled.contains("Responding"),
        "{settled}"
    );
    assert_eq!(state.task_console().records.len(), 3);
    state.open_task_category(TaskCategory::Agents);
    assert_eq!(state.task_console().visible_records().len(), 3);
}

#[test]
fn active_agent_strip_caps_rows_and_keeps_unicode_names_timers_and_overflow_accessible() {
    for (width, height) in [(60, 24), (110, 42), (180, 48)] {
        let (mut state, source) = state();
        *source.rows.lock().unwrap() = (0..7)
            .map(|index| {
                let mut child = row(
                    &format!("{index}"),
                    if index == 1 {
                        TaskStatus::Failed
                    } else {
                        TaskStatus::Running
                    },
                );
                child.label = format!("Agent {index} 東京 é 🧪 long label");
                child.telemetry.elapsed_ms = [Some(1_200), Some(65_000), None][index % 3];
                child.telemetry.initial_prompt =
                    Some("DO NOT DUMP THIS INITIAL PROMPT /private/very/long/path".into());
                child
            })
            .collect();
        state.refresh_tasks();
        let buffer = frame(&mut state, width, height);
        let rendered = text(&buffer);
        assert!(rendered.contains("+3 more"), "{rendered}");
        assert_eq!(
            state
                .task_console()
                .records
                .iter()
                .filter(|record| record.active_child())
                .count(),
            6
        );
        assert_eq!(state.task_console().pending_issue_count(), 1);
        assert!(!rendered.contains("DO NOT DUMP"), "{rendered}");
        let hits = state
            .task_console()
            .hits
            .iter()
            .filter(|(_, hit)| matches!(hit, TaskHit::Switch(_)))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 3);
        for (rect, hit) in hits {
            let TaskHit::Switch(key) = hit else {
                unreachable!()
            };
            let child = state
                .task_console()
                .records
                .iter()
                .find(|child| &child.key == key)
                .unwrap();
            assert_eq!(rect.x, 0, "agent rings align to the terminal left edge");
            let line = (rect.x..rect.right())
                .map(|x| buffer[(x, rect.y)].symbol())
                .collect::<String>();
            let timer = match child.telemetry.elapsed_ms {
                Some(1_200) => "1s",
                Some(65_000) => "1m 5s",
                _ => "—",
            };
            assert!(line.ends_with(timer), "{line}");
            assert_eq!(
                buffer[(2, rect.y)].symbol(),
                "A",
                "names have a shared left column"
            );
            assert!(
                !line.contains(child.status_label()) && !line.contains("Read"),
                "no middle action or status word: {line}"
            );
            assert_eq!(buffer[(0, rect.y)].symbol(), "○");
            let expected_color = if child.status == TaskStatus::Failed {
                state.styles().error()
            } else {
                state.styles().accent()
            };
            assert_eq!(buffer[(0, rect.y)].fg, expected_color);
        }
        let overflow = state
            .task_console()
            .hits
            .iter()
            .rev()
            .find(|(_, hit)| *hit == TaskHit::Category(TaskCategory::Agents))
            .unwrap()
            .0;
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: overflow.x,
            row: overflow.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(state.task_console().category, TaskCategory::Agents);
        assert_eq!(state.task_console().visible_records().len(), 7);
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut state, KeyCode::Down, KeyModifiers::NONE);
        for _ in 0..7 {
            press(&mut state, KeyCode::Down, KeyModifiers::NONE);
        }
        let focused = text(&frame(&mut state, width, height));
        assert!(focused.contains("Agent 6"), "{focused}");
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            state.task_console().selected.as_ref(),
            Some(&TaskKey("child:6".into()))
        );
    }
}

#[test]
fn parent_footer_preserves_context_and_git_across_sizes_and_settlement() {
    use heycode_tui::workspace_context::{
        PullRequestContext, WorkspaceContext, WorkspaceContextState,
    };
    for (width, height) in [(60, 24), (110, 42), (180, 48)] {
        let mut state = AppState::new(
            "model-with-a-long-optional-display-name",
            "/workspace".into(),
        );
        let initial = text(&frame(&mut state, width, height));
        assert!(!initial.contains("limit unknown"), "{width}: {initial}");
        assert!(!initial.contains("context —"), "{width}: {initial}");
        state.context_tokens = Some(8_000);
        state.context_window = Some(100_000);
        state.set_workspace_context(WorkspaceContextState::Ready(WorkspaceContext {
            branch: "feature/agents".into(),
            dirty: true,
            pull_request: PullRequestContext::None,
        }));
        for active in [false, true, false] {
            if active {
                state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
            } else {
                state.apply(&heycode_agent::UiEvent::TurnFinished {
                    reason: "completed".into(),
                    usage: None,
                    context_tokens: None,
                });
            }
            let rendered = text(&frame(&mut state, width, height));
            assert!(rendered.contains("feature/agents *"), "{width}: {rendered}");
            assert!(rendered.contains("8k/100k"), "{width}: {rendered}");
            assert!(rendered.contains("92% left"), "{width}: {rendered}");
        }
        state.set_workspace_context(WorkspaceContextState::NotRepository);
        let no_git = text(&frame(&mut state, width, height));
        assert!(!no_git.contains("No Git repository"), "{no_git}");
        assert!(!no_git.contains("feature/agents"), "{no_git}");
        assert!(no_git.contains("8k/100k"), "{no_git}");
    }
}

#[test]
fn parent_activity_uses_current_turn_and_cancellation_request_until_settled() {
    let mut state = AppState::new("model", "/workspace".into());
    state.apply(&heycode_agent::UiEvent::ToolStarted {
        name: "bash".into(),
        args: serde_json::json!({"command":"old command"}),
    });
    let idle = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(idle.contains("activity: ready"), "{idle}");
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 2 });
    let working = text(&frame(&mut state, 110, 42));
    assert!(working.contains("Working"), "{working}");
    assert!(!working.contains("Running: old command"), "{working}");
    state.interrupt_fn = Some(Box::new(|| {}));
    press(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    let cancelling = text(&frame(&mut state, 110, 42));
    assert!(cancelling.contains("Cancelling"), "{cancelling}");
    assert!(
        state.has_active_turn(),
        "interrupt request is not settlement"
    );
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .into_text()
            .contains("activity: cancelling")
    );
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "aborted".into(),
        usage: None,
        context_tokens: None,
    });
    assert!(!state.has_active_turn());
    assert!(!text(&frame(&mut state, 110, 42)).contains("Cancelling"));
}

#[test]
fn parent_footer_keeps_context_confidence_and_compaction_evidence_at_sixty_columns() {
    let mut state = AppState::new("model", "/workspace".into());
    state.apply(&heycode_agent::UiEvent::RuntimeContextMeasured {
        resolved_model: Some("resolved-model".into()),
        tokens: 7_000,
        context_window: 100_000,
    });
    let exact = text(&frame(&mut state, 60, 24));
    assert!(
        exact.contains("ctx:7k/100k") && !exact.contains("ctx:~"),
        "{exact}"
    );
    let mut budget = heycode_llm::context_budget(
        "provider".into(),
        "model".into(),
        &heycode_llm::EnvelopeTotal::Estimated(75_000),
        Some(100_000),
        0,
        1_000,
        0.8,
        true,
    );
    for (confidence, expected) in [
        (
            heycode_core::ContextConfidence::Estimated,
            "ctx:~75k/100k (~25% left)",
        ),
        (
            heycode_core::ContextConfidence::AtLeast,
            "ctx:≥75k/100k (≤25% left)",
        ),
    ] {
        budget.confidence = confidence;
        state.apply(&heycode_agent::UiEvent::ContextBudgetChanged {
            budget: budget.clone(),
        });
        let buffer = frame(&mut state, 60, 24);
        assert_footer_color(&buffer, "ctx:", state.styles().warn());
        let rendered = text(&buffer);
        assert!(
            rendered.contains(expected) && rendered.contains("compaction soon"),
            "{rendered}"
        );
    }
    budget.activity = heycode_core::ContextActivity::Failed;
    state.apply(&heycode_agent::UiEvent::ContextBudgetChanged {
        budget: budget.clone(),
    });
    let buffer = frame(&mut state, 60, 24);
    assert!(text(&buffer).contains("compaction failed"));
    assert_footer_color(&buffer, "ctx:", state.styles().error());
    budget.window = None;
    budget.activity = heycode_core::ContextActivity::Ready;
    state.apply(&heycode_agent::UiEvent::ContextBudgetChanged { budget });
    assert!(text(&frame(&mut state, 60, 24)).contains("limit unknown"));
}

fn assert_footer_color(
    buffer: &ratatui::buffer::Buffer,
    label: &str,
    color: ratatui::style::Color,
) {
    let cells = buffer
        .content
        .windows(label.chars().count())
        .rev()
        .find(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>() == label)
        .unwrap_or_else(|| panic!("missing footer label: {label}"));
    assert!(
        cells.iter().all(|cell| cell.fg == color),
        "wrong color for {label}: {cells:?}"
    );
}

#[test]
fn parent_footer_colors_follow_theme_and_context_pressure() {
    use heycode_tui::workspace_context::{
        PullRequestContext, WorkspaceContext, WorkspaceContextState,
    };
    for (theme_id, no_color) in [
        ("heycode-dark", false),
        ("heycode-light", false),
        ("heycode-dark", true),
    ] {
        let theme = heycode_ui::theme::builtin_themes()
            .unwrap()
            .into_iter()
            .find(|theme| theme.id().as_str() == theme_id)
            .unwrap();
        let environment = heycode_ui::terminal::TerminalEnvironment::new()
            .with_term(Some("xterm-256color"))
            .with_colorterm(Some("truecolor"))
            .with_no_color(no_color.then_some("1"));
        let mut state = AppState::new("footer-model", "/workspace".into());
        state.apply_terminal(
            heycode_ui::terminal::TerminalCapabilities::detect(&environment),
            &theme,
        );
        state.context_tokens = Some(8_000);
        state.context_window = Some(100_000);
        state.usage = Some(heycode_core::TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
        });
        state.set_workspace_context(WorkspaceContextState::Ready(WorkspaceContext {
            branch: "feature/footer".into(),
            dirty: true,
            pull_request: PullRequestContext::None,
        }));
        let buffer = frame(&mut state, 180, 24);
        assert_footer_color(&buffer, "feature/footer *", state.styles().panel_title());
        assert_footer_color(&buffer, "footer-model", state.styles().accent());
        assert_footer_color(&buffer, "ctx:", state.styles().success());
        assert_footer_color(&buffer, "in:100 out:20", state.styles().code());
        if no_color {
            assert_eq!(state.styles().accent(), ratatui::style::Color::Reset);
            assert_eq!(state.styles().success(), ratatui::style::Color::Reset);
        }
        state.context_tokens = Some(99_000);
        assert_footer_color(&frame(&mut state, 60, 24), "ctx:", state.styles().warn());
        state.context_tokens = Some(100_000);
        assert_footer_color(&frame(&mut state, 60, 24), "ctx:", state.styles().error());
        state.context_window = None;
        assert_footer_color(
            &frame(&mut state, 60, 24),
            "limit unknown",
            state.styles().dim(),
        );
    }
}

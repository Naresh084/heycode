//! Plan-mode end-to-end: guard enforcement, human-approved exit, /plan command.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, AutoApprove, DenyAll, PlanSelection, agent_options_plugin, agent_plugin,
    approval_plugin, commands_plugin, plan_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, LlmSelection, Provider, ProviderInfo, StreamChunk, llm_plugin,
    model_catalog_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_session::{Session, SessionEventKind, session_plugin};
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

struct Recording {
    inner: FakeProvider,
    sink: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
}

impl Provider for Recording {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.sink.lock().unwrap().push(request.clone());
        self.inner.stream(request)
    }
}

fn call(id: &str, tool: &str, args: serde_json::Value) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index: 0,
        id: Some(id.to_owned()),
        name: Some(tool.to_owned()),
        arguments_delta: args.to_string(),
    }
}

fn stop_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

struct World {
    ctx: heycode_core::Context,
    agent: Arc<heycode_agent::Agent>,
    session: Arc<std::sync::Mutex<Session>>,
    requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
}

async fn world(
    dir: &std::path::Path,
    approval: Arc<dyn heycode_agent::ApprovalPolicy>,
    scripts: Vec<Vec<StreamChunk>>,
) -> World {
    world_with_approval_plugin(dir, approval_plugin(approval), scripts).await
}

async fn world_with_approval_plugin(
    dir: &std::path::Path,
    approval: Box<dyn Plugin>,
    scripts: Vec<Vec<StreamChunk>>,
) -> World {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(scripts),
        sink: requests.clone(),
    });
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(dir.to_path_buf()),
        prompt_plugin(),
        execution_plugin(),
        heycode_exec::terminal_registry_plugin(),
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
        approval,
        commands_plugin(),
        plan_plugin(),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
        heycode_agent::execution_jobs_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    World {
        ctx,
        agent,
        session,
        requests,
    }
}

#[tokio::test]
async fn explicit_review_commits_exact_permission_before_actual_shell_mutation() {
    use heycode_agent::{
        ApprovalPolicy, ApprovalPolicyKind, InteractiveApproval, PlanReviewDecision,
        SwitchableApproval, UiEvent,
    };
    for (decision, target_mode, record_decision) in [
        (
            PlanReviewDecision::AcceptedEdits,
            ApprovalPolicyKind::AcceptedEdits,
            "accepted_edits",
        ),
        (
            PlanReviewDecision::DefaultPermissions,
            ApprovalPolicyKind::Ask,
            "default",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        std::fs::write(&target, "a\n").unwrap();
        let scripts = vec![
            vec![
                call(
                    "blocked",
                    "edit",
                    serde_json::json!({ "path": target, "old_string": "a", "new_string": "b" }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("standing by"),
            vec![
                call(
                    "plan",
                    "exit_plan_mode",
                    serde_json::json!({ "plan": "# The full plan\n## Objective\nChange f.txt\n## Validation\nRead it back" }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            vec![
                call(
                    "shell",
                    "bash",
                    serde_json::json!({ "command": format!("printf 'b\\n' > '{}'", target.display()) }),
                ),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("done"),
        ];
        let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
        interactive.set_plan_review_available(true);
        let switch = Arc::new(SwitchableApproval::new(
            Arc::new(AutoApprove),
            Some(Arc::new(interactive.clone())),
        ));
        let w = world_with_approval_plugin(
            dir.path(),
            heycode_agent::switchable_approval_plugin(switch.clone(), interactive.clone()),
            scripts,
        )
        .await;
        let plan = w
            .ctx
            .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
            .unwrap();
        plan.0.set(true).await.unwrap();
        assert_eq!(switch.kind(), ApprovalPolicyKind::Plan);
        assert!(switch.switch(ApprovalPolicyKind::FullAccess).is_err());
        w.agent.send("try to edit").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a\n");
        assert!(
            w.requests.lock().unwrap()[0]
                .messages
                .iter()
                .any(|message| message.content.contains("PLAN MODE ACTIVE"))
        );

        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_in_callback = seen.clone();
        let policy = switch.clone();
        let session = w.session.clone();
        w.ctx.events.on::<UiEvent>(move |event| match event {
            UiEvent::PlanReviewRequested { id, plan } => {
                assert!(plan.contains("## Validation\nRead it back"));
                assert!(interactive.answer_plan(*id, decision.clone()));
            }
            UiEvent::ApprovalRequested { id, name, .. } => {
                assert_eq!(name, "bash", "exit_plan_mode must not create a generic approval");
                assert_eq!(policy.kind(), target_mode, "new policy must already be actual runtime authority");
                assert!(session.lock().unwrap().events().iter().any(|event| matches!(&event.kind, SessionEventKind::PlanReview { decision, .. } if decision == record_decision)));
                seen_in_callback.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                interactive.answer(*id, true);
            }
            _ => {}
        });
        w.agent.send("present and implement").await.unwrap();
        assert!(!plan.0.active());
        assert_eq!(switch.kind(), target_mode);
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "b\n");
        let folded =
            heycode_agent::PlanMode::from_log(&w.session, heycode_core::EventBus::default());
        assert!(!folded.active());
        assert_eq!(folded.review().unwrap().decision, record_decision);
    }
}

#[tokio::test]
async fn rejected_plan_refuses_mutations_before_ordinary_approval() {
    use heycode_agent::{InteractiveApproval, PlanReviewDecision, SwitchableApproval, UiEvent};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("must-not-exist.txt");
    let scripts = vec![
        vec![
            call(
                "plan",
                "exit_plan_mode",
                serde_json::json!({"plan": "# Review\nWrite a file"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        vec![
            call(
                "write",
                "write",
                serde_json::json!({"path": target, "content": "forbidden"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        vec![
            call(
                "shell",
                "bash",
                serde_json::json!({"command": format!("touch '{}'", target.display())}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        vec![
            call("unknown", "third_party_mutation", serde_json::json!({})),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("remaining in Plan"),
    ];
    let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
    interactive.set_plan_review_available(true);
    let switch = Arc::new(SwitchableApproval::new(
        Arc::new(interactive.clone()),
        Some(Arc::new(interactive.clone())),
    ));
    let w = world_with_approval_plugin(
        dir.path(),
        heycode_agent::switchable_approval_plugin(switch, interactive.clone()),
        scripts,
    )
    .await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    let prompts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = prompts.clone();
    w.ctx.events.on::<UiEvent>(move |event| match event {
        UiEvent::PlanReviewRequested { id, .. } => {
            assert!(interactive.answer_plan(
                *id,
                PlanReviewDecision::StayInPlan {
                    feedback: "Keep researching".into()
                }
            ));
        }
        UiEvent::ApprovalRequested { id, .. } => {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Even a permissive responder must never be asked about these calls.
            interactive.answer(*id, true);
        }
        _ => {}
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.agent.send("review then try mutations"),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(plan.0.active());
    assert!(!target.exists());
    assert_eq!(prompts.load(std::sync::atomic::Ordering::SeqCst), 0);
    let session = w.session.lock().unwrap();
    for call_id in ["write", "shell", "unknown"] {
        assert!(session.events().iter().any(|event| matches!(
            &event.kind,
            SessionEventKind::ToolResult { call_id: result_id, is_error: true, content, .. }
                if result_id.as_str() == call_id && content.contains("plan mode is active")
        )));
    }
}

#[tokio::test]
async fn rejected_plan_keeps_planning_and_guard_stays_up() {
    let scripts = vec![
        stop_text("thinking"), // turn under plan
        vec![
            call(
                "p1",
                "exit_plan_mode",
                serde_json::json!({"plan": "# Plan B\ndo less"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("kept planning"), // exit denied result fed back
    ];
    let dir = tempfile::tempdir().unwrap();
    let w = world(dir.path(), Arc::new(DenyAll), scripts).await;
    let commands = w
        .ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    commands
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "on")
        .await
        .unwrap();

    w.agent.send("research").await.unwrap();
    w.agent.send("presenting plan").await.unwrap();

    let s = w.session.lock().unwrap_or_else(|e| e.into_inner());
    let still_active = s.events().iter().rev().find_map(|e| match e.kind {
        SessionEventKind::PlanMode { active } => Some(active),
        _ => None,
    });
    assert_eq!(still_active, Some(true), "denied exit keeps plan mode on");
    assert!(s.events().iter().any(|e| matches!(
        &e.kind,
        SessionEventKind::ToolResult { is_error: false, content, .. }
            if content.contains("stay_in_plan")
    )));
}

#[tokio::test]
async fn mid_turn_selection_waits_for_pre_step_while_idle_selection_commits_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let w = world(
        dir.path(),
        Arc::new(AutoApprove),
        vec![stop_text("planned")],
    )
    .await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    {
        let mut session = w.session.lock().unwrap_or_else(|error| error.into_inner());
        session
            .append(SessionEventKind::TurnStart { turn: 0 })
            .unwrap();
    }
    assert_eq!(plan.0.set(true).await.unwrap(), PlanSelection::Queued);
    assert!(
        plan.0.active(),
        "new mutations block immediately during entry"
    );
    assert_eq!(plan.0.pending(), Some(true));
    {
        let mut session = w.session.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            session
                .events()
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::PlanMode { active: true }))
        );
        session
            .append(SessionEventKind::TurnEnd {
                turn: 0,
                reason: heycode_session::TurnEndReason::Stop,
            })
            .unwrap();
    }

    w.agent.send("make a plan").await.unwrap();
    assert!(plan.0.active());
    assert_eq!(plan.0.pending(), None);
    let events = w
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .to_vec();
    let mode = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::PlanMode { active: true }))
        .unwrap();
    let step = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::StepStart { turn: 1, .. }))
        .unwrap();
    assert!(mode < step);

    assert!(
        plan.0.set(false).await.is_err(),
        "manual off must not bypass plan review"
    );
    assert!(plan.0.active());
}

#[tokio::test]
async fn plan_command_optional_message_is_owned_by_the_domain_and_logged_once() {
    let dir = tempfile::tempdir().unwrap();
    let w = world(
        dir.path(),
        Arc::new(AutoApprove),
        vec![stop_text("plan ready")],
    )
    .await;
    let commands = w
        .ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    commands
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "inspect the session lifecycle")
        .await
        .unwrap();

    let session = w.session.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| matches!(
                &event.kind,
                SessionEventKind::UserMessage { text }
                    if text == "inspect the session lifecycle"
            ))
            .count(),
        1
    );
    assert!(
        session
            .events()
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::PlanMode { active: true }))
    );
}

#[tokio::test]
async fn explicit_user_mode_change_exits_idle_and_queued_plan_without_accepting_a_proposal() {
    use heycode_agent::{
        ApprovalPolicy, ApprovalPolicyKind, InteractiveApproval, SwitchableApproval,
    };
    for busy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
        let switch = Arc::new(SwitchableApproval::new(
            Arc::new(AutoApprove),
            Some(Arc::new(interactive.clone())),
        ));
        let w = world_with_approval_plugin(
            dir.path(),
            heycode_agent::switchable_approval_plugin(switch.clone(), interactive),
            vec![],
        )
        .await;
        let plan = w
            .ctx
            .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
            .unwrap();
        if busy {
            w.session
                .lock()
                .unwrap()
                .append(SessionEventKind::TurnStart { turn: 1 })
                .unwrap();
        }
        plan.0.set(true).await.unwrap();
        assert!(plan.0.active());
        assert!(
            switch.switch(ApprovalPolicyKind::FullAccess).is_err(),
            "automatic transitions still require review"
        );
        assert!(switch.switch_by_user(ApprovalPolicyKind::Auto).is_err());
        assert!(
            plan.0.active(),
            "an unavailable target cannot change the current mode"
        );
        switch.switch_by_user(ApprovalPolicyKind::Ask).unwrap();
        assert!(!plan.0.active());
        assert_eq!(plan.0.pending(), None);
        assert_eq!(switch.kind(), ApprovalPolicyKind::Ask);
        assert!(
            plan.0.review().is_none(),
            "leaving manually is not accepting a proposal"
        );
        assert!(
            !heycode_agent::plan::PlanMode::from_log(&w.session, heycode_core::EventBus::default())
                .active()
        );
    }
}

#[tokio::test]
async fn manual_mode_change_withdraws_a_pending_review_and_late_acceptance_cannot_restore_plan() {
    use heycode_agent::{
        ApprovalPolicy, ApprovalPolicyKind, InteractiveApproval, SwitchableApproval, UiEvent,
    };
    let dir = tempfile::tempdir().unwrap();
    let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
    interactive.set_plan_review_available(true);
    let switch = Arc::new(SwitchableApproval::new(
        Arc::new(AutoApprove),
        Some(Arc::new(interactive.clone())),
    ));
    let w = world_with_approval_plugin(
        dir.path(),
        heycode_agent::switchable_approval_plugin(switch.clone(), interactive.clone()),
        vec![],
    )
    .await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    let request_id = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let observed = request_id.clone();
    let policy = switch.clone();
    w.ctx.events.on::<UiEvent>(move |event| {
        if let UiEvent::PlanReviewRequested { id, .. } = event {
            observed.store(*id, std::sync::atomic::Ordering::SeqCst);
            policy.switch_by_user(ApprovalPolicyKind::Ask).unwrap();
        }
    });
    let tools = w
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tools.get("exit_plan_mode").unwrap().run(
            serde_json::json!({"plan":"# Implementation plan\nRead, edit, and test the fixture."}),
            &heycode_tools::ToolCtx::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output["status"], "mode_changed");
    assert_eq!(switch.kind(), ApprovalPolicyKind::Ask);
    assert!(!interactive.answer_plan(
        request_id.load(std::sync::atomic::Ordering::SeqCst),
        heycode_agent::PlanReviewDecision::AcceptedEdits
    ));
    assert!(
        !heycode_agent::plan::PlanMode::from_log(&w.session, heycode_core::EventBus::default())
            .active()
    );
    plan.0.set(true).await.unwrap();
    w.ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "off")
        .await
        .unwrap();
    assert!(!plan.0.active());
    assert_eq!(switch.kind(), ApprovalPolicyKind::Ask);
}

/// A layer that counts how many times the chain reached it, then delegates.
struct Tally(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl heycode_core::Layer<heycode_tools::PreToolDecision> for Tally {
    async fn handle(
        &self,
        input: &mut heycode_tools::PreToolDecision,
        mut next: heycode_core::Next<'_, heycode_tools::PreToolDecision>,
    ) -> anyhow::Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        next.run(input).await
    }
}

/// Principle #6 / GOTCHAS #3: a layer that does not deny MUST delegate, or every
/// layer registered after it is silently skipped for every tool call.
#[tokio::test]
async fn plan_guard_delegates_to_downstream_layers_when_it_does_not_deny() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = vec![
        vec![
            call("r1", "glob", serde_json::json!({"pattern": "*.txt"})),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("done"),
    ];
    let w = world(dir.path(), Arc::new(AutoApprove), scripts).await;

    let seam = w
        .ctx
        .get::<heycode_core::Waterfall<heycode_tools::PreToolDecision>>(
            heycode_tools::SEAM_PRE_TOOL,
        )
        .unwrap();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    seam.push_shared(Tally(seen.clone()));

    // Plan mode OFF and a read-only tool: the guard has no reason to deny, so
    // the chain must continue past it.
    w.agent.send("find the text files").await.unwrap();

    assert_eq!(
        seen.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "PlanGuard swallowed the chain: downstream layers never ran"
    );
}

/// The deny path is the ONE case where returning without `next` is correct.
#[tokio::test]
async fn plan_guard_short_circuits_downstream_layers_when_it_denies() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("f.txt");
    std::fs::write(&target, "a\n").unwrap();
    let t = target.to_str().unwrap().to_owned();
    let scripts = vec![
        vec![
            call(
                "e1",
                "edit",
                serde_json::json!({"path": t, "old_string": "a", "new_string": "b"}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("blocked"),
    ];
    let w = world(dir.path(), Arc::new(AutoApprove), scripts).await;

    let seam = w
        .ctx
        .get::<heycode_core::Waterfall<heycode_tools::PreToolDecision>>(
            heycode_tools::SEAM_PRE_TOOL,
        )
        .unwrap();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    seam.push_shared(Tally(seen.clone()));

    let commands = w
        .ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    commands
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "on")
        .await
        .unwrap();

    w.agent.send("try to edit").await.unwrap();

    assert_eq!(
        seen.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a denial must short-circuit; no downstream layer may resurrect it"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "a\n");
}

/// Plan mode's contract is "no mutation", not "not these three names".
/// `background_shell` and `background_terminal` are shell tools composed in
/// the same default profile as the guard, so the identical command the guard
/// denies as `bash` must not execute through them either.
#[tokio::test]
async fn plan_mode_denies_the_background_shell_alias_of_bash() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("victim.txt");
    std::fs::write(&target, "clean\n").unwrap();
    let t = target.to_str().unwrap().to_owned();

    let scripts = vec![
        vec![
            call(
                "b1",
                "background_shell",
                serde_json::json!({"command": format!("printf 'via-background-shell' > {t}")}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("blocked"),
        vec![
            call(
                "b2",
                "background_terminal",
                serde_json::json!({"command": format!("printf 'via-background-terminal' > {t}")}),
            ),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("blocked"),
    ];
    let w = world(dir.path(), Arc::new(AutoApprove), scripts).await;
    let commands = w
        .ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    commands
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "on")
        .await
        .unwrap();

    for prompt in ["write it in the background", "try a PTY instead"] {
        w.agent.send(prompt).await.unwrap();
    }
    // Background jobs are spawned tasks: give a wrongly admitted one time to
    // land so the assertion cannot pass by winning a race.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "clean\n",
        "plan mode admitted a shell write through a background job"
    );
    let session = w.session.lock().unwrap_or_else(|error| error.into_inner());
    let denials = session
        .events()
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                SessionEventKind::ToolResult { is_error: true, content, .. }
                    if content.contains("plan mode is active")
            )
        })
        .count();
    assert_eq!(denials, 2, "both background aliases must be denied");
}

/// The inversion itself: plan mode admits an allow-list, so a tool nobody
/// taught the guard about is denied rather than exempt. Every name below is a
/// tool the shipped composition can register; the mutating column is exactly
/// the set that escaped the old three-name deny-list.
#[tokio::test]
async fn plan_mode_admits_only_read_only_tools_and_denies_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let w = world(dir.path(), Arc::new(AutoApprove), Vec::new()).await;
    let seam = w
        .ctx
        .get::<heycode_core::Waterfall<heycode_tools::PreToolDecision>>(
            heycode_tools::SEAM_PRE_TOOL,
        )
        .unwrap();
    let commands = w
        .ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    commands
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "on")
        .await
        .unwrap();

    let read_only = [
        "exit_plan_mode",
        "glob",
        "grep",
        "list_jobs",
        "list_tasks",
        "load_skill",
        "lsp_definition",
        "lsp_diagnostics",
        "lsp_references",
        "lsp_servers",
        "read",
        "schedule_list",
        "task",
        "terminal_list",
        "terminal_read",
        "task_create",
        "task_get",
        "task_list",
        "task_update",
        "understand_image",
        "web_fetch",
        "web_search",
    ];
    let mutating = [
        "background_shell",
        "background_terminal",
        "bash",
        "cancel_job",
        "edit",
        "goal",
        "interrupt_task",
        "mcp__server__write_file",
        "schedule_create",
        "schedule_delete",
        "send_message",
        "team",
        "terminal_kill",
        "terminal_open",
        "terminal_resize",
        "terminal_write",
        "tool_invented_next_week",
        "workflow",
        "write",
    ];

    for (name, action, allowed) in [
        ("agent_control", "list", true),
        ("agent_control", "wait", true),
        ("agent_control", "send", false),
        ("agent_control", "interrupt", false),
        ("agent_control", "archive", false),
        ("agent_control", "restore", false),
        ("job_control", "list", true),
        ("job_control", "output", true),
        ("job_control", "cancel", false),
        ("job_control", "unknown", false),
        ("agent_control", "", false),
    ] {
        let mut decision = heycode_tools::PreToolDecision {
            call: heycode_tools::ToolCallInput {
                name: name.into(),
                args: serde_json::json!({"action":action}),
            },
            verdict: heycode_tools::Verdict::Allow,
        };
        seam.run(&mut decision).await.unwrap();
        assert_eq!(
            matches!(decision.verdict, heycode_tools::Verdict::Allow),
            allowed,
            "{name} {action}"
        );
    }

    for name in read_only {
        let mut decision = heycode_tools::PreToolDecision {
            call: heycode_tools::ToolCallInput {
                name: name.to_owned(),
                args: serde_json::json!({}),
            },
            verdict: heycode_tools::Verdict::Allow,
        };
        seam.run(&mut decision).await.unwrap();
        assert!(
            matches!(decision.verdict, heycode_tools::Verdict::Allow),
            "plan mode must still admit the read-only tool `{name}`"
        );
    }
    for name in mutating {
        let mut decision = heycode_tools::PreToolDecision {
            call: heycode_tools::ToolCallInput {
                name: name.to_owned(),
                args: serde_json::json!({}),
            },
            verdict: heycode_tools::Verdict::Allow,
        };
        seam.run(&mut decision).await.unwrap();
        match decision.verdict {
            heycode_tools::Verdict::Deny { reason } => assert!(
                reason.contains("plan mode is active"),
                "`{name}` was denied for the wrong reason: {reason}"
            ),
            heycode_tools::Verdict::Allow => {
                panic!("plan mode admitted the mutating tool `{name}`")
            }
        }
    }
}

async fn exit_direct(
    w: &World,
    document: &str,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<serde_json::Value, heycode_tools::ToolError> {
    w.ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .get("exit_plan_mode")
        .unwrap()
        .run(
            serde_json::json!({ "plan": document }),
            &heycode_tools::ToolCtx {
                cwd: std::env::current_dir().unwrap(),
                cancellation,
            },
        )
        .await
}

#[tokio::test]
async fn full_access_and_generic_allow_cannot_accept_a_plan() {
    let dir = tempfile::tempdir().unwrap();
    let w = world(dir.path(), Arc::new(AutoApprove), Vec::new()).await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    let result = exit_direct(
        &w,
        "# Plan\nNo auto-accept",
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "stay_in_plan");
    assert!(plan.0.active());
    assert_eq!(plan.0.review().unwrap().decision, "stay_in_plan");
}

#[tokio::test]
async fn explicit_feedback_survives_revision_resume_and_reopening_saved_review() {
    use heycode_agent::{InteractiveApproval, PlanReviewDecision, SwitchableApproval, UiEvent};
    let dir = tempfile::tempdir().unwrap();
    let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
    interactive.set_plan_review_available(true);
    let switch = Arc::new(SwitchableApproval::new(
        Arc::new(AutoApprove),
        Some(Arc::new(interactive.clone())),
    ));
    let w = world_with_approval_plugin(
        dir.path(),
        heycode_agent::switchable_approval_plugin(switch, interactive.clone()),
        Vec::new(),
    )
    .await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    let policy = interactive.clone();
    w.ctx.events.on::<UiEvent>(move |event| {
        if let UiEvent::PlanReviewRequested { id, .. } = event {
            // A generic allow and an allow-session grant are not plan decisions.
            policy.answer(*id, true);
            assert!(policy.plan_is_pending(*id));
            policy.answer_plan(
                *id,
                PlanReviewDecision::StayInPlan {
                    feedback: "Keep the migration reversible".into(),
                },
            );
        }
    });
    let document = format!(
        "# Detailed plan\n{}\n## Final validation\nVerify the very last step",
        "A complete paragraph.\n".repeat(900)
    );
    let result = exit_direct(&w, &document, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["feedback"], "Keep the migration reversible");
    let folded = heycode_agent::PlanMode::from_log(&w.session, heycode_core::EventBus::default());
    assert!(folded.active());
    assert_eq!(folded.review().unwrap().plan, document);
    assert_eq!(
        folded.review().unwrap().feedback,
        "Keep the migration reversible"
    );
    w.ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("plan")
        .unwrap()
        .unwrap()
        .execute(&w.agent, "review")
        .await
        .unwrap();
    assert!(plan.0.active());
    assert_eq!(plan.0.review().unwrap().plan, document);
    assert_eq!(w.session.lock().unwrap().events().iter().filter(|event| matches!(&event.kind, SessionEventKind::PlanReview { decision, .. } if decision == "pending")).count(), 2);
}

#[tokio::test]
async fn cancellation_withdraws_review_and_retains_pending_proposal_read_only() {
    use heycode_agent::{InteractiveApproval, SwitchableApproval, UiEvent};
    let dir = tempfile::tempdir().unwrap();
    let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
    interactive.set_plan_review_available(true);
    let switch = Arc::new(SwitchableApproval::new(
        Arc::new(AutoApprove),
        Some(Arc::new(interactive.clone())),
    ));
    let w = world_with_approval_plugin(
        dir.path(),
        heycode_agent::switchable_approval_plugin(switch, interactive.clone()),
        Vec::new(),
    )
    .await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let cancel = cancellation.clone();
    let id = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let output_id = id.clone();
    w.ctx.events.on::<UiEvent>(move |event| {
        if let UiEvent::PlanReviewRequested { id, .. } = event {
            output_id.store(*id, std::sync::atomic::Ordering::SeqCst);
            cancel.cancel();
        }
    });
    let result = exit_direct(&w, "# Pending proposal\nKeep me", cancellation)
        .await
        .unwrap();
    assert_eq!(result["status"], "stay_in_plan");
    assert!(!interactive.plan_is_pending(id.load(std::sync::atomic::Ordering::SeqCst)));
    assert!(plan.0.active());
    let folded = heycode_agent::PlanMode::from_log(&w.session, heycode_core::EventBus::default());
    assert!(folded.active());
    assert_eq!(folded.review().unwrap().plan, "# Pending proposal\nKeep me");
}

struct FailedTransition;
#[async_trait::async_trait]
impl heycode_agent::ApprovalPolicy for FailedTransition {
    async fn decide(&self, _: &heycode_tools::ToolCallInput) -> heycode_tools::Verdict {
        heycode_tools::Verdict::Allow
    }
    async fn review_plan(
        &self,
        _: &str,
        _: tokio_util::sync::CancellationToken,
    ) -> heycode_agent::PlanReviewDecision {
        heycode_agent::PlanReviewDecision::AcceptedEdits
    }
    fn commit_plan_transition(
        &self,
        _: heycode_agent::ApprovalPolicyKind,
        _: &mut (dyn FnMut() -> Result<(), String> + Send),
    ) -> Result<(), String> {
        Err("injected permission transition failure".into())
    }
}

#[tokio::test]
async fn failed_transition_preserves_pending_review_and_denies_next_actual_edit() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("untouched.txt");
    std::fs::write(&target, "before").unwrap();
    let w = world(dir.path(), Arc::new(FailedTransition), vec![
        vec![call("edit", "edit", serde_json::json!({ "path": target, "old_string": "before", "new_string": "after" })), StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls)], stop_text("blocked")
    ]).await;
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    assert!(
        exit_direct(
            &w,
            "# Plan\nMake the change",
            tokio_util::sync::CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("injected")
    );
    assert!(plan.0.active());
    assert_eq!(plan.0.review().unwrap().decision, "pending");
    let folded = heycode_agent::PlanMode::from_log(&w.session, heycode_core::EventBus::default());
    assert!(folded.active());
    assert_eq!(folded.review().unwrap().decision, "pending");
    w.agent
        .send("try mutation after failed transition")
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(target).unwrap(), "before");
}

#[tokio::test]
async fn entering_plan_cancels_and_joins_actual_background_mutations_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("must-not-exist.txt");
    let ready = dir.path().join("ready.txt");
    let w = world(dir.path(), Arc::new(AutoApprove), Vec::new()).await;
    let execution = w
        .ctx
        .get::<heycode_agent::ExecutionJobService>(heycode_agent::SERVICE_EXECUTION_JOBS)
        .unwrap();
    let job = execution
        .start_shell(
            "mutating background",
            heycode_exec::ShellRequest::new(format!(
                "touch '{}'; sleep 1; touch '{}'",
                ready.display(),
                target.display()
            ))
            .unwrap(),
            heycode_session::InboxDelivery::FollowUp,
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !ready.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    assert_eq!(plan.0.set(true).await.unwrap(), PlanSelection::Committed);
    assert!(plan.0.active());
    assert_eq!(plan.0.pending(), None);
    let jobs = w
        .ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    assert_eq!(
        jobs.wait_for_settlement(&job).await.unwrap(),
        heycode_agent::JobOutcome::Cancelled
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(
        !target.exists(),
        "Plan entry returned before the process tree was settled"
    );
}

#[test]
fn plan_review_records_reopen_from_disk_with_the_entire_document_and_feedback() {
    for decision in ["pending", "stay_in_plan", "accepted_edits", "default"] {
        let root = tempfile::tempdir().unwrap();
        let document = format!(
            "# Resume plan\n{}\nLAST-PROPOSAL-LINE",
            "Retained detailed steps.\n".repeat(1000)
        );
        let path = {
            let mut session = Session::create(root.path()).unwrap();
            session
                .append(SessionEventKind::PlanMode { active: true })
                .unwrap();
            session
                .append(SessionEventKind::PlanReview {
                    plan: document.clone(),
                    decision: decision.into(),
                    feedback: "Preserve this reviewer feedback".into(),
                })
                .unwrap();
            session.path().parent().unwrap().to_path_buf()
        };
        let session = Arc::new(std::sync::Mutex::new(
            Session::open_for_writing(&path).unwrap(),
        ));
        let plan = heycode_agent::PlanMode::from_log(&session, heycode_core::EventBus::default());
        assert_eq!(
            plan.active(),
            matches!(decision, "pending" | "stay_in_plan")
        );
        let review = plan.review().unwrap();
        assert_eq!(review.decision, decision);
        assert_eq!(review.plan, document);
        assert_eq!(review.feedback, "Preserve this reviewer feedback");
    }
}

#[test]
fn failed_atomic_policy_commit_keeps_the_original_policy_and_never_publishes_target() {
    use heycode_agent::{ApprovalPolicy, ApprovalPolicyKind, SwitchableApproval};
    let policy = SwitchableApproval::new(Arc::new(AutoApprove), Some(Arc::new(DenyAll)));
    let mut called = false;
    let result = policy.commit_plan_transition(ApprovalPolicyKind::Ask, &mut || {
        called = true;
        Err("simulated durable append failure".into())
    });
    assert!(called);
    assert!(result.is_err());
    assert_eq!(policy.kind(), ApprovalPolicyKind::FullAccess);
    assert!(
        policy
            .commit_plan_transition(ApprovalPolicyKind::FullAccess, &mut || panic!(
                "invalid target must not commit"
            ))
            .is_err()
    );
}

#[tokio::test]
async fn plan_blocks_shell_hooks_in_both_composition_orders_and_settles_inflight_hooks() {
    use heycode_hooks::{Hook, HookAction, HookEvent, HookOutcome, HookPhase, HookService};
    use tokio_util::sync::CancellationToken;
    for hooks_first in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("hook-started");
        let finished = root.path().join("hook-finished");
        let mut plugins: Vec<Box<dyn Plugin>> = vec![
            session_plugin(root.path().to_path_buf()),
            prompt_plugin(),
            execution_plugin(),
            heycode_exec::terminal_registry_plugin(),
            heycode_exec::local_filesystem_plugin(),
            heycode_web::web_registry_plugin(),
            heycode_native_tools::native_tools_plugin(),
            tools_plugin(heycode_tools::ToolsConfig::default()),
            approval_plugin(Arc::new(AutoApprove)),
            commands_plugin(),
        ];
        let hooks = heycode_hooks::hooks_plugin(heycode_trust::WorkspaceTrustDecision::Trusted);
        if hooks_first {
            plugins.push(hooks);
            plugins.push(plan_plugin());
        } else {
            plugins.push(plan_plugin());
            plugins.push(hooks);
        }
        let ctx = compose(&plugins).unwrap();
        let hooks = ctx
            .get::<HookService>(heycode_hooks::SERVICE_HOOKS)
            .unwrap();
        hooks.register(
            &ctx,
            Hook {
                owner: "plan-hook-test".into(),
                phase: HookPhase::Pre,
                event: HookEvent::ToolUse,
                action: HookAction::Command(format!(
                    "touch '{}'; sleep 0.3; touch '{}'",
                    ready.display(),
                    finished.display()
                )),
                project_scoped: false,
            },
        );
        let running = hooks.clone();
        let operation = tokio::spawn(async move {
            running
                .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let plan = ctx
            .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
            .unwrap();
        assert_eq!(plan.0.set(true).await.unwrap(), PlanSelection::Committed);
        assert!(
            finished.exists(),
            "entry must settle the already-started hook before announcing read-only"
        );
        assert!(operation.await.unwrap()[0].proceeds());
        std::fs::remove_file(&finished).unwrap();
        let refused = hooks
            .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
            .await;
        assert!(matches!(&refused[0], HookOutcome::Refused { .. }));
        assert!(!finished.exists(), "a shell hook must not bypass Plan mode");
    }
}

#[tokio::test]
async fn plan_entry_waits_for_worker_tail_after_its_terminal_row_and_timeout_stays_read_only() {
    let root = tempfile::tempdir().unwrap();
    let w = world(root.path(), Arc::new(AutoApprove), Vec::new()).await;
    let jobs = w
        .ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let tail_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_release = release.clone();
    let worker_finished = tail_finished.clone();
    let worker_agent = w.agent.clone();
    let worker_jobs = (*jobs).clone();
    let id = jobs
        .spawn(
            "worker tail",
            heycode_session::InboxDelivery::FollowUp,
            move |id, _| async move {
                worker_agent
                    .settle_job(
                        &worker_jobs,
                        &id,
                        &heycode_agent::JobSettlement::new(
                            heycode_agent::JobOutcome::Completed,
                            "row settled, tail still executing",
                        )
                        .unwrap(),
                    )
                    .unwrap();
                worker_release.notified().await;
                worker_finished.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .unwrap();
    jobs.wait_for_settlement(&id).await.unwrap();
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    let error = plan.0.set(true).await.unwrap_err();
    assert!(error.to_string().contains("active execution"));
    assert!(plan.0.active());
    assert_eq!(plan.0.pending(), Some(true));
    assert!(!tail_finished.load(std::sync::atomic::Ordering::SeqCst));
    let folded = heycode_agent::PlanMode::from_log(&w.session, heycode_core::EventBus::default());
    assert!(
        folded.active(),
        "timed-out entry must remain read-only on resume"
    );
    release.notify_one();
    jobs.wait_for_task_exit(&id).await.unwrap();
    assert_eq!(plan.0.set(true).await.unwrap(), PlanSelection::Committed);
    assert!(tail_finished.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(plan.0.pending(), None);
}

#[tokio::test]
async fn code_mode_plan_refusal_precedes_workflow_approval() {
    struct CountApproval(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl heycode_agent::ApprovalPolicy for CountApproval {
        async fn decide(&self, _: &heycode_tools::ToolCallInput) -> heycode_tools::Verdict {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            heycode_tools::Verdict::Allow
        }
    }
    let root = tempfile::tempdir().unwrap();
    let approvals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut w = world(
        root.path(),
        Arc::new(CountApproval(approvals.clone())),
        Vec::new(),
    )
    .await;
    heycode_agent::code_mode_plugin().apply(&mut w.ctx).unwrap();
    w.agent.enter_plan().await.unwrap();
    let path = root.path().join("must-not-exist");
    for (name, args) in [
        ("write", serde_json::json!({"path":path,"content":"denied"})),
        ("run_code", serde_json::json!({"source":"return 1;"})),
    ] {
        let error = w
            .agent
            .execute_workflow_tool(
                name.into(),
                args,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("plan mode is active"),
            "{error:#}"
        );
    }
    let question = w
        .agent
        .execute_workflow_tool(
            "ask_user_question_async".into(),
            serde_json::json!({"question":"Which format should the plan use?"}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(question["status"], "pending");
    assert_eq!(w.agent.async_questions().unwrap().len(), 1);
    assert_eq!(approvals.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!path.exists());
    assert!(
        w.ctx
            .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
            .unwrap()
            .0
            .active()
    );
    w.ctx.shutdown();
}

#[tokio::test]
async fn model_plan_entry_is_durable_idempotent_and_owned() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = world(dir.path(), Arc::new(AutoApprove), Vec::new()).await;
    let tools = w
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let entry = tools
        .get("enter_plan_mode")
        .expect("model-facing plan entry is registered");
    let result = entry
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(result["status"], "committed");
    assert_eq!(result["plan_mode"], true);
    let durable_entry_events = w
        .session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::PlanMode { active: true }))
        .count();
    assert!(durable_entry_events > 0);
    let result = entry
        .run(serde_json::json!({}), &heycode_tools::ToolCtx::default())
        .await
        .unwrap();
    assert_eq!(result["status"], "already_active");
    assert_eq!(
        w.session
            .lock()
            .unwrap()
            .events()
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::PlanMode { active: true }))
            .count(),
        durable_entry_events
    );
    w.ctx.shutdown();
    assert!(tools.get("enter_plan_mode").is_none());
    assert!(tools.get("exit_plan_mode").is_none());
}

#[tokio::test]
async fn model_plan_entry_blocks_later_mutations_in_the_same_batch() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("must-not-exist.txt");
    let mut write = call(
        "write-after-plan",
        "write",
        serde_json::json!({"path":target,"content":"unsafe","overwrite":true}),
    );
    if let StreamChunk::ToolCallDelta { index, .. } = &mut write {
        *index = 1;
    }
    let w = world(
        dir.path(),
        Arc::new(AutoApprove),
        vec![
            vec![
                call("enter-plan", "enter_plan_mode", serde_json::json!({})),
                write,
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            stop_text("Planning only."),
        ],
    )
    .await;
    w.agent
        .send("Plan first, then consider changes.")
        .await
        .unwrap();
    assert!(!target.exists());
    let plan = w
        .ctx
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    assert!(plan.0.active());
    let requests = w.requests.lock().unwrap();
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.content.contains("plan mode is active"))
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.content.contains("PLAN MODE ACTIVE"))
    );
}

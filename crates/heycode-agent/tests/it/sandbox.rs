//! End-to-end confinement below the model-facing shell Consumer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AutoApprove, CompactionPolicy, agent_plugin, approval_plugin, commands_plugin,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{LlmSelection, StreamChunk, llm_plugin, model_catalog_plugin};
use heycode_prompt::prompt_plugin;
use heycode_session::session_plugin;
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

fn sandboxed_execution_plugin(
    root: &std::path::Path,
    backend: Arc<dyn heycode_exec::Sandbox>,
) -> Box<dyn heycode_core::Plugin> {
    let config = heycode_exec::LocalShellConfig::platform(
        std::env::current_dir().unwrap(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    let sandbox = heycode_exec::SandboxService::new(
        heycode_exec::SandboxMode::WorkspaceWrite,
        root.canonicalize().unwrap(),
        Some(backend),
    )
    .unwrap();
    heycode_exec::local_execution_plugin_with_sandbox(config, sandbox)
}

fn call(id: &str, cmd: &str) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index: 0,
        id: Some(id.to_owned()),
        name: Some("bash".to_owned()),
        arguments_delta: serde_json::json!({"command": cmd}).to_string(),
    }
}

fn stop_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

#[tokio::test]
async fn bash_writes_inside_workspace_succeed_and_outside_fail() {
    let Some(sb) = heycode_sandbox::SeatbeltSandbox::new().ok().map(Arc::new) else {
        return; // non-macOS dev host: seatbelt provider refuses loudly there
    };
    let dir = tempfile::tempdir().unwrap();
    let inside = dir.path().join("allowed.txt");
    let outside = std::env::temp_dir().join(format!("heycode-e2e-deny-{}", std::process::id()));
    let _ = std::fs::remove_file(&outside);

    let scripts = vec![
        vec![
            call("c1", &format!("printf ok > {}", inside.display())),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("inside done"),
        vec![
            call("c2", &format!("printf no > {}", outside.display())),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ],
        stop_text("outside blocked"),
    ];

    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(dir.path().to_path_buf()),
        prompt_plugin(),
        sandboxed_execution_plugin(dir.path(), sb),
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
            vec![Arc::new(FakeProvider::new(scripts))],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        options_plugin(heycode_agent::AgentOptions {
            compaction: CompactionPolicy::default(),
            max_task_depth: 3,
            auto_title: false,
            cwd: Some(dir.path().to_path_buf()),
        }),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    agent.send("write the allowed file").await.unwrap();
    assert_eq!(std::fs::read_to_string(&inside).unwrap(), "ok");

    agent.send("try escaping").await.unwrap();
    assert!(
        !outside.exists(),
        "sandboxed bash must not write outside the workspace"
    );
    {
        let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
        // Nonzero exit is a RESULT, not a tool error: the confinement spoke.
        assert!(s.events().iter().any(|e| matches!(
            &e.kind,
            heycode_session::SessionEventKind::ToolResult { is_error: false, content, .. }
                if content.contains("not permitted") && content.contains("[exit code: 1]")
        )));
    }
    let _ = std::fs::remove_file(&outside);
}

#[tokio::test]
async fn turn_cancellation_settles_the_shell_descendant_tree_before_abort_returns() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("shell-ready");
    let release = dir.path().join("release-descendant");
    let survived = dir.path().join("descendant-survived");
    let command = format!(
        "touch {}; (while [ ! -f {} ]; do sleep 0.05; done; printf survived > {}) & sleep 30",
        ready.display(),
        release.display(),
        survived.display()
    );
    let scripts = vec![vec![
        call("cancel-shell", &command),
        StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
    ]];
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(dir.path().to_path_buf()),
        prompt_plugin(),
        execution_plugin(),
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
            vec![Arc::new(FakeProvider::new(scripts))],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        options_plugin(heycode_agent::AgentOptions {
            compaction: CompactionPolicy::default(),
            max_task_depth: 3,
            auto_title: false,
            cwd: Some(dir.path().to_path_buf()),
        }),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let running_agent = agent.clone();
    let turn = tokio::spawn(async move { running_agent.send("run long shell").await });
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(ready.exists(), "shell never reached its readiness marker");
    agent.token().cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(2), turn)
        .await
        .expect("cancelled shell turn must settle")
        .unwrap()
        .unwrap();
    assert_eq!(report.reason, "aborted");
    std::fs::write(&release, "release").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!survived.exists(), "shell descendant escaped cancellation");
}

fn options_plugin(options: heycode_agent::AgentOptions) -> Box<dyn Plugin> {
    struct Opts(heycode_agent::AgentOptions);
    impl Plugin for Opts {
        fn name(&self) -> &'static str {
            "agent-options"
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
            ctx.provide(
                heycode_agent::SERVICE_AGENT_OPTIONS,
                "agent-options",
                self.0.clone(),
            )
        }
    }
    Box::new(Opts(options))
}

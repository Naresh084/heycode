//! R12 local SDK process, protocol, event and lifecycle fixtures.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use heycode_exec::{SandboxMode, SandboxService, local_subprocess_plugin, sandbox_service_plugin};
use heycode_llm::CapabilitySupport;
use heycode_runtime::{
    AccountStatus, AgentRuntimeRegistry, RuntimeConfiguration, RuntimeErrorCode, RuntimeEventKind,
    RuntimeInput, RuntimeStart, runtime_registry_plugin,
};
use heycode_runtime_deepseek_harness::{
    DEEPSEEK_HARNESS_RUNTIME_ID, DeepSeekHarnessRuntimeConfig, PINNED_DSH_SDK_VERSION,
    deepseek_harness_runtime_plugin,
};
use tokio_util::sync::CancellationToken;

fn fixture_program(root: &Path) -> PathBuf {
    let program = root.join("dsh-sdk-fixture");
    std::fs::write(
        &program,
        include_bytes!("fixtures/dsh-sdk-fixture.sh").as_slice(),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&program, permissions).unwrap();
    program
}

fn config(program: &Path, environment: Vec<(OsString, OsString)>) -> DeepSeekHarnessRuntimeConfig {
    DeepSeekHarnessRuntimeConfig::new()
        .with_program(program.as_os_str())
        .unwrap()
        .with_environment(environment)
        .unwrap()
}

fn plugins(
    workspace: &Path,
    config: DeepSeekHarnessRuntimeConfig,
) -> Vec<Box<dyn heycode_core::Plugin>> {
    vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, workspace, None).unwrap()),
        local_subprocess_plugin(),
        deepseek_harness_runtime_plugin(config),
    ]
}

#[tokio::test]
async fn local_sdk_turn_normalizes_events_and_closes_quiescently() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let mut context =
        heycode_core::compose(&plugins(&workspace, config(&program, vec![]))).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let descriptor = runtime.descriptor();
    assert_eq!(
        descriptor.capabilities().models,
        CapabilitySupport::Unsupported
    );
    for support in [
        descriptor.capabilities().resume,
        descriptor.capabilities().fork,
        descriptor.capabilities().steer,
        descriptor.capabilities().follow_up,
        descriptor.capabilities().permissions,
        descriptor.capabilities().questions,
        descriptor.capabilities().compaction,
    ] {
        assert_eq!(support, CapabilitySupport::Unsupported);
    }
    assert_eq!(
        descriptor.configuration_capabilities().model,
        CapabilitySupport::Supported
    );
    for support in [
        descriptor.configuration_capabilities().system_prompt,
        descriptor.configuration_capabilities().tools,
        descriptor.configuration_capabilities().reasoning_effort,
    ] {
        assert_eq!(support, CapabilitySupport::Unsupported);
    }

    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("dsh-fixture-session"),
        &workspace,
    )
    .unwrap()
    .with_model("deepseek-v4-pro")
    .unwrap();
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    let mut events = session.subscribe();
    let turn = session
        .send(
            RuntimeInput::new("run the local fixture").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(turn.as_str(), "7");

    let mut observed = Vec::new();
    while let Ok(Some(Ok(event))) =
        tokio::time::timeout(Duration::from_millis(50), events.next()).await
    {
        let settled = matches!(event.kind(), RuntimeEventKind::TurnFinished { .. });
        observed.push(event);
        if settled {
            break;
        }
    }
    assert_eq!(
        observed
            .iter()
            .map(heycode_runtime::RuntimeEvent::sequence)
            .collect::<Vec<_>>(),
        (0..observed.len() as u64).collect::<Vec<_>>()
    );
    assert!(observed.iter().any(
        |event| matches!(event.kind(), RuntimeEventKind::CommentaryDelta { text } if text == "working")
    ));
    assert!(
        observed
            .iter()
            .any(|event| matches!(event.kind(), RuntimeEventKind::ToolCall { .. }))
    );
    assert!(
        observed
            .iter()
            .any(|event| matches!(event.kind(), RuntimeEventKind::ToolResult { .. }))
    );
    assert!(observed.iter().any(
        |event| matches!(event.kind(), RuntimeEventKind::Usage { usage, .. } if usage.prompt_tokens == 9 && usage.completion_tokens == 2)
    ));
    assert!(observed.iter().any(
        |event| matches!(event.kind(), RuntimeEventKind::FinalMessage { text } if text == "done")
    ));
    session.close(CancellationToken::new()).await.unwrap();
    session.close(CancellationToken::new()).await.unwrap();
    context.shutdown();
    assert!(runtimes.ids().unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_configuration_is_named_before_the_sdk_process_launches() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let mut context =
        heycode_core::compose(&plugins(&workspace, config(&program, vec![]))).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let configuration = RuntimeConfiguration::new()
        .with_system_prompt("exact prompt")
        .unwrap()
        .with_tools(Vec::new())
        .unwrap()
        .with_reasoning_effort("high")
        .unwrap();
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("rejected-controls"),
        &workspace,
    )
    .unwrap()
    .with_configuration(configuration);
    let error = runtime
        .start(request, CancellationToken::new())
        .await
        .err()
        .expect("unsupported controls must fail before launch");
    assert_eq!(error.code(), RuntimeErrorCode::Unsupported);
    assert_eq!(
        error.message(),
        "runtime configuration fields are unsupported: system_prompt, tools, reasoning_effort"
    );
    context.shutdown();
}

#[tokio::test]
async fn version_and_event_sequence_drift_fail_closed_without_provider_bodies() {
    for environment in [
        vec![(
            OsString::from("DSH_FIXTURE_VERSION"),
            OsString::from("0.0.2"),
        )],
        vec![(OsString::from("DSH_FIXTURE_GAP"), OsString::from("1"))],
    ] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let program = fixture_program(&workspace);
        let mut context =
            heycode_core::compose(&plugins(&workspace, config(&program, environment))).unwrap();
        let runtimes = context
            .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
            .unwrap();
        let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
        let request = RuntimeStart::new(
            heycode_core::SessionId::from_raw("dsh-fixture-session"),
            &workspace,
        )
        .unwrap();
        match runtime.start(request, CancellationToken::new()).await {
            Err(error) => {
                assert_eq!(error.code(), RuntimeErrorCode::Protocol);
                assert!(!format!("{error:?}").contains(PINNED_DSH_SDK_VERSION));
            }
            Ok(session) => {
                let error = session
                    .send(
                        RuntimeInput::new("trigger sequence validation").unwrap(),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(error.code(), RuntimeErrorCode::Protocol);
                session.close(CancellationToken::new()).await.unwrap();
            }
        }
        context.shutdown();
    }
}

#[tokio::test]
async fn caller_cancellation_reaps_the_uncancellable_wire_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let environment = vec![(OsString::from("DSH_FIXTURE_HANG"), OsString::from("1"))];
    let mut context =
        heycode_core::compose(&plugins(&workspace, config(&program, environment))).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("dsh-fixture-session"),
        &workspace,
    )
    .unwrap();
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    let caller = CancellationToken::new();
    let send_session = Arc::clone(&session);
    let send_caller = caller.clone();
    let send = tokio::spawn(async move {
        send_session
            .send(RuntimeInput::new("hang").unwrap(), send_caller)
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    caller.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), send)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Cancelled
    );
    assert_eq!(
        session
            .send(
                RuntimeInput::new("cannot reuse closed wire").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Closed
    );
    assert_eq!(
        session
            .cancel(CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Unsupported
    );
    session.close(CancellationToken::new()).await.unwrap();
    context.shutdown();
}

#[tokio::test]
async fn pre_receipt_idle_status_is_outside_the_owned_prompt_interval() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let environment = vec![(
        OsString::from("DSH_FIXTURE_INITIAL_IDLE"),
        OsString::from("1"),
    )];
    let mut context =
        heycode_core::compose(&plugins(&workspace, config(&program, environment))).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("dsh-fixture-session"),
        &workspace,
    )
    .unwrap();
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        session
            .send(
                RuntimeInput::new("own only receipt through next idle").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .as_str(),
        "7"
    );
    session.close(CancellationToken::new()).await.unwrap();
    context.shutdown();
}

#[tokio::test]
async fn missing_optional_runtime_registers_truthful_unavailable_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let missing = workspace.join("missing-dsh-jsonrpc-agent");
    let mut context =
        heycode_core::compose(&plugins(&workspace, config(&missing, vec![]))).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    assert_eq!(
        runtime
            .account(CancellationToken::new())
            .await
            .unwrap()
            .status(),
        AccountStatus::Unavailable
    );
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("missing-runtime"),
        &workspace,
    )
    .unwrap();
    let error = runtime
        .start(request, CancellationToken::new())
        .await
        .err()
        .expect("missing optional runtime must not start a session");
    assert_eq!(error.code(), RuntimeErrorCode::Unavailable);
    context.shutdown();
}

#[tokio::test]
async fn executable_replacement_after_registration_fails_before_initialize() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let mut context =
        heycode_core::compose(&plugins(&workspace, config(&program, vec![]))).unwrap();

    let mut replacement = include_bytes!("fixtures/dsh-sdk-fixture.sh").to_vec();
    replacement.extend_from_slice(b"\n# byte-distinct compatible replacement\n");
    std::fs::write(&program, replacement).unwrap();

    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("replaced-dsh-runtime"),
        &workspace,
    )
    .unwrap();
    let error = match runtime.start(request, CancellationToken::new()).await {
        Err(error) => Some(error),
        Ok(session) => {
            let _closed = session.close(CancellationToken::new()).await;
            None
        }
    };
    context.shutdown();
    assert_eq!(
        error
            .expect("a replaced executable inherited the pinned Harness identity")
            .code(),
        RuntimeErrorCode::Protocol
    );
}

#[tokio::test]
async fn reviewed_artifact_replacement_after_registration_fails_before_initialize() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let artifact = workspace.join("reviewed-sdk-bundle.js");
    std::fs::write(&artifact, b"reviewed-sdk-generation-one").unwrap();
    let config = config(&program, vec![])
        .with_artifacts(vec![artifact.clone()])
        .unwrap();
    let mut context = heycode_core::compose(&plugins(&workspace, config)).unwrap();

    std::fs::write(&artifact, b"replaced-sdk-generation-two").unwrap();

    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("replaced-dsh-artifact"),
        &workspace,
    )
    .unwrap();
    let error = match runtime.start(request, CancellationToken::new()).await {
        Err(error) => Some(error),
        Ok(session) => {
            let _closed = session.close(CancellationToken::new()).await;
            None
        }
    };
    context.shutdown();
    assert_eq!(
        error
            .expect("a replaced SDK artifact inherited the pinned Harness identity")
            .code(),
        RuntimeErrorCode::Protocol
    );
}

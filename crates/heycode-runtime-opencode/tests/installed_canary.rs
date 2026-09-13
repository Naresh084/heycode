//! Explicitly gated, credential-blind installed OpenCode ACP canary.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use futures::StreamExt as _;
use heycode_exec::{SandboxMode, SandboxService, local_subprocess_plugin, sandbox_service_plugin};
use heycode_runtime::{
    AgentRuntimeRegistry, RuntimeEventKind, RuntimeFinishReason, RuntimeInput, RuntimeStart,
    runtime_registry_plugin,
};
use heycode_runtime_opencode::{
    OPENCODE_GLM_5_3_FLASH_MODEL_ID, OPENCODE_RUNTIME_ID, OpenCodeRuntimeConfig,
    opencode_environment_snapshot, opencode_runtime_plugin,
};
use tokio_util::sync::CancellationToken;

const OPENCODE_CREDENTIAL_BLIND_CANARY_MODEL_ID: &str = "opencode/mimo-v2.5-free";

fn account_only_environment(workspace: &Path) -> Vec<(OsString, OsString)> {
    const ISOLATED_NAMES: &[&str] = &[
        "TEMP",
        "TMP",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
    ];
    let mut environment = opencode_environment_snapshot()
        .into_iter()
        .filter(|(name, _)| {
            let normalized = name.to_string_lossy().to_ascii_uppercase();
            !ISOLATED_NAMES.contains(&normalized.as_str())
        })
        .collect::<Vec<_>>();
    assert!(environment.iter().any(|(name, _)| {
        matches!(
            name.to_string_lossy().to_ascii_uppercase().as_str(),
            "HOME" | "XDG_DATA_HOME"
        )
    }));
    for (name, directory) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("TMPDIR", "tmp"),
    ] {
        let path = workspace.join(directory);
        std::fs::create_dir(&path).unwrap();
        environment.push((OsString::from(name), path.into_os_string()));
    }
    let opencode_config = workspace.join("config/opencode");
    std::fs::create_dir(&opencode_config).unwrap();
    std::fs::write(
        opencode_config.join("opencode.json"),
        b"{\"$schema\":\"https://opencode.ai/config.json\",\"permission\":\"deny\"}\n",
    )
    .unwrap();
    environment.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(environment.iter().all(|(name, _)| {
        let normalized = name.to_string_lossy().to_ascii_uppercase();
        !["API_KEY", "TOKEN", "AUTH", "PROXY"]
            .iter()
            .any(|fragment| normalized.contains(fragment))
    }));
    environment
}

fn credential_blind_denied_environment(workspace: &Path) -> Vec<(OsString, OsString)> {
    let mut environment = Vec::new();
    for (name, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("TMPDIR", "tmp"),
    ] {
        let path = workspace.join(directory);
        std::fs::create_dir(&path).unwrap();
        environment.push((OsString::from(name), path.into_os_string()));
    }
    environment.extend([
        (
            OsString::from("OPENCODE_CONFIG_CONTENT"),
            OsString::from(
                "{\"$schema\":\"https://opencode.ai/config.json\",\"permission\":\"deny\"}",
            ),
        ),
        (
            OsString::from("OPENCODE_DISABLE_PROJECT_CONFIG"),
            OsString::from("1"),
        ),
    ]);
    environment.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(environment.iter().all(|(name, _)| {
        let normalized = name.to_string_lossy().to_ascii_uppercase();
        !["API_KEY", "TOKEN", "AUTH", "PROXY"]
            .iter()
            .any(|fragment| normalized.contains(fragment))
    }));
    environment
}

async fn run_content_withheld_turn(
    executable: OsString,
    workspace: &Path,
    environment: Vec<(OsString, OsString)>,
    model: &str,
) {
    let config = OpenCodeRuntimeConfig::new(workspace)
        .unwrap()
        .with_program(executable)
        .unwrap()
        .with_environment(environment)
        .unwrap();
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, workspace, None).unwrap()),
        local_subprocess_plugin(),
        opencode_runtime_plugin(config),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(OPENCODE_RUNTIME_ID).unwrap().unwrap();
    let catalog = tokio::time::timeout(
        Duration::from_secs(30),
        runtime.models(CancellationToken::new()),
    )
    .await
    .expect("installed OpenCode catalog probe timed out")
    .unwrap();
    let model_present = catalog.models.iter().any(|row| row.id == model);
    if !model_present {
        context.shutdown();
        assert!(
            model_present,
            "installed OpenCode lacks the reviewed canary model"
        );
    }

    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("heycode-opencode-live"),
        workspace,
    )
    .unwrap()
    .with_model(model)
    .unwrap();
    let session = tokio::time::timeout(
        Duration::from_secs(30),
        runtime.start(request, CancellationToken::new()),
    )
    .await
    .expect("installed OpenCode session initialization timed out")
    .unwrap();
    let mut events = session.subscribe();
    let send_ok = tokio::time::timeout(
        Duration::from_secs(90),
        session.send(
            RuntimeInput::new("Reply with a brief acknowledgement. Do not use tools.").unwrap(),
            CancellationToken::new(),
        ),
    )
    .await
    .is_ok_and(|result| result.is_ok());

    let mut final_nonempty = false;
    let mut tool_activity = false;
    let mut stream_failed = false;
    let mut finish_reason = None;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(5), events.next()).await {
        let event = match event {
            Ok(event) => event,
            Err(_) => {
                stream_failed = true;
                break;
            }
        };
        match event.kind() {
            RuntimeEventKind::FinalMessage { text } => final_nonempty = !text.trim().is_empty(),
            RuntimeEventKind::ToolCall { .. }
            | RuntimeEventKind::ToolResult { .. }
            | RuntimeEventKind::PermissionRequested { .. } => tool_activity = true,
            RuntimeEventKind::TurnFinished { reason, .. } => {
                finish_reason = Some(*reason);
                break;
            }
            _ => {}
        }
    }
    let close_ok = tokio::time::timeout(
        Duration::from_secs(10),
        session.close(CancellationToken::new()),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    context.shutdown();

    assert!(send_ok, "OpenCode canary turn did not settle successfully");
    assert!(!stream_failed, "OpenCode canary event stream failed");
    assert!(!tool_activity, "OpenCode canary attempted tool activity");
    assert!(final_nonempty, "OpenCode canary returned no final text");
    assert_eq!(finish_reason, Some(RuntimeFinishReason::Stop));
    assert!(
        close_ok,
        "OpenCode canary session did not close quiescently"
    );
}

#[tokio::test]
async fn installed_opencode_catalog_canary_is_explicit_and_credential_blind() {
    if std::env::var("HEYCODE_OPENCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let executable = std::env::var_os("HEYCODE_OPENCODE_EXECUTABLE")
        .expect("HEYCODE_OPENCODE_EXECUTABLE is required for the explicit canary");
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let home = workspace.join("home");
    let config_home = workspace.join("config");
    let data_home = workspace.join("data");
    let cache_home = workspace.join("cache");
    for path in [&home, &config_home, &data_home, &cache_home] {
        std::fs::create_dir(path).unwrap();
    }
    let environment = vec![
        (OsString::from("HOME"), home.into_os_string()),
        (
            OsString::from("XDG_CONFIG_HOME"),
            config_home.into_os_string(),
        ),
        (OsString::from("XDG_DATA_HOME"), data_home.into_os_string()),
        (
            OsString::from("XDG_CACHE_HOME"),
            cache_home.into_os_string(),
        ),
    ];
    let config = OpenCodeRuntimeConfig::new(&workspace)
        .unwrap()
        .with_program(executable)
        .unwrap()
        .with_environment(environment)
        .unwrap();
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        opencode_runtime_plugin(config),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(OPENCODE_RUNTIME_ID).unwrap().unwrap();
    let catalog = tokio::time::timeout(
        Duration::from_secs(30),
        runtime.models(CancellationToken::new()),
    )
    .await
    .expect("installed OpenCode catalog probe timed out")
    .unwrap();
    assert_eq!(catalog.provider.id, OPENCODE_RUNTIME_ID);
    assert!(!catalog.models.is_empty());
    context.shutdown();
}

#[tokio::test]
async fn installed_opencode_glm_turn_is_official_tool_safe_and_content_withheld() {
    if std::env::var("HEYCODE_OPENCODE_GLM_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let executable = std::env::var_os("HEYCODE_OPENCODE_EXECUTABLE")
        .expect("HEYCODE_OPENCODE_EXECUTABLE is required for the explicit canary");
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    run_content_withheld_turn(
        executable,
        &workspace,
        account_only_environment(&workspace),
        OPENCODE_GLM_5_3_FLASH_MODEL_ID,
    )
    .await;
}

#[tokio::test]
async fn installed_opencode_official_free_turn_is_credential_blind_and_content_withheld() {
    if std::env::var("HEYCODE_OPENCODE_FREE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let executable = std::env::var_os("HEYCODE_OPENCODE_EXECUTABLE")
        .expect("HEYCODE_OPENCODE_EXECUTABLE is required for the explicit canary");
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    run_content_withheld_turn(
        executable,
        &workspace,
        credential_blind_denied_environment(&workspace),
        OPENCODE_CREDENTIAL_BLIND_CANARY_MODEL_ID,
    )
    .await;
}

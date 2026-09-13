//! R10 production process/plugin fixtures.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use heycode_exec::{SandboxMode, SandboxService, local_subprocess_plugin, sandbox_service_plugin};
use heycode_runtime::{AccountStatus, AgentRuntimeRegistry, runtime_registry_plugin};
use heycode_runtime_opencode::{
    OPENCODE_GLM_5_3_FLASH_MODEL_ID, OPENCODE_RUNTIME_ID, OpenCodeRuntimeConfig,
    PINNED_OPENCODE_VERSION, opencode_runtime_plugin,
};
use tokio_util::sync::CancellationToken;

fn fixture_program(root: &Path) -> PathBuf {
    let program = root.join("opencode-fixture");
    std::fs::write(
        &program,
        include_bytes!("fixtures/opencode-fixture.sh").as_slice(),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&program, permissions).unwrap();
    program
}

fn config(workspace: &Path, program: &Path, version: &str) -> OpenCodeRuntimeConfig {
    OpenCodeRuntimeConfig::new(workspace)
        .unwrap()
        .with_program(program.as_os_str())
        .unwrap()
        .with_environment(vec![(
            OsString::from("OPENCODE_FIXTURE_VERSION"),
            OsString::from(version),
        )])
        .unwrap()
}

#[tokio::test]
async fn plugin_registers_version_bound_runtime_and_disposes_its_effect() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        opencode_runtime_plugin(config(&workspace, &program, PINNED_OPENCODE_VERSION)),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    assert_eq!(runtimes.ids().unwrap(), [OPENCODE_RUNTIME_ID]);

    let runtime = runtimes.get(OPENCODE_RUNTIME_ID).unwrap().unwrap();
    let catalog = runtime.models(CancellationToken::new()).await.unwrap();
    assert_eq!(catalog.provider.id, OPENCODE_RUNTIME_ID);
    assert_eq!(catalog.models.len(), 2);
    assert_eq!(catalog.models[0].id, OPENCODE_GLM_5_3_FLASH_MODEL_ID);

    context.shutdown();
    assert!(runtimes.ids().unwrap().is_empty());
}

#[tokio::test]
async fn version_drift_fails_before_the_acp_connection_is_published() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        opencode_runtime_plugin(config(&workspace, &program, "1.18.20")),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(OPENCODE_RUNTIME_ID).unwrap().unwrap();
    let error = runtime.models(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Protocol);
    assert!(!format!("{error:?}").contains("1.18.20"));
    context.shutdown();
}

#[tokio::test]
async fn executable_replacement_after_registration_fails_before_spawn() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        opencode_runtime_plugin(config(&workspace, &program, PINNED_OPENCODE_VERSION)),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    std::fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(OPENCODE_RUNTIME_ID).unwrap().unwrap();
    assert_eq!(
        runtime
            .models(CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        heycode_runtime::RuntimeErrorCode::Protocol
    );
    context.shutdown();
}

#[tokio::test]
async fn missing_optional_installation_registers_truthful_unavailable_runtime() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let missing = workspace.join("missing-opencode");
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        opencode_runtime_plugin(config(&workspace, &missing, PINNED_OPENCODE_VERSION)),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(OPENCODE_RUNTIME_ID).unwrap().unwrap();
    assert_eq!(
        runtime
            .account(CancellationToken::new())
            .await
            .unwrap()
            .status(),
        AccountStatus::Unavailable
    );
    assert_eq!(
        runtime
            .models(CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        heycode_runtime::RuntimeErrorCode::Unavailable
    );
    context.shutdown();
}

#[tokio::test]
async fn initialized_agent_identity_must_match_the_version_bound_executable() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let program = fixture_program(&workspace);
    let config = OpenCodeRuntimeConfig::new(&workspace)
        .unwrap()
        .with_program(program.as_os_str())
        .unwrap()
        .with_environment(vec![
            (
                OsString::from("OPENCODE_FIXTURE_VERSION"),
                OsString::from(PINNED_OPENCODE_VERSION),
            ),
            (
                OsString::from("OPENCODE_FIXTURE_AGENT_VERSION"),
                OsString::from("1.18.20"),
            ),
        ])
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
    assert_eq!(
        runtime
            .models(CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        heycode_runtime::RuntimeErrorCode::Protocol
    );
    context.shutdown();
}

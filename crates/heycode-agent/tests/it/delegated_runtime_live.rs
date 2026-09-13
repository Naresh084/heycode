//! Explicitly gated R05/R08 installed-subscription canaries.
//!
//! These tests never read credential values. Each runtime is allowed to use
//! only its official installed account store, receives a fixed tool-free prompt
//! in an empty temporary workspace, runs under deny-all delegated permissions
//! and writes its heycode child log only beneath the temporary root.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    DenyAll, RuntimeSubagentProvider, SubagentContinuation, SubagentProvider as _, SubagentRequest,
    SubagentSeed,
};
use heycode_runtime::AgentRuntime;
use tokio_util::sync::CancellationToken;

fn request(prompt: &str) -> SubagentRequest {
    SubagentRequest::new(
        "installed runtime canary",
        prompt,
        SubagentSeed::Fresh,
        SubagentContinuation::OneShot,
        0,
    )
    .unwrap()
}

#[tokio::test]
async fn live_installed_codex_ephemeral_subagent_is_explicitly_gated() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    const CANARY: &str = "R05_CODEX_SUBAGENT_OK";
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = heycode_runtime_codex::CodexAppServerConfig::new(
        "codex",
        &workspace,
        heycode_runtime_codex::codex_environment_snapshot(),
        heycode_runtime_codex::CodexClientInfo::new("heycode", "heycode", "0.1.0").unwrap(),
    )
    .unwrap();
    let runtime: Arc<dyn AgentRuntime> = Arc::new(
        heycode_runtime_codex::CodexRuntime::new(heycode_exec::SubprocessService::local(), config)
            .unwrap(),
    );
    let provider = RuntimeSubagentProvider::new(
        runtime,
        Arc::new(DenyAll),
        "codex",
        "Codex delegated agent",
        sessions,
        workspace,
        2,
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let cancel_on_timeout = cancellation.clone();
    let mut task = tokio::spawn(async move {
        provider
            .start(
                request(&format!(
                    "Reply with exactly {CANARY}. Do not use tools, files, shell, network, or MCP."
                )),
                cancellation,
            )
            .await
    });
    let result = match tokio::time::timeout(std::time::Duration::from_secs(120), &mut task).await {
        Ok(joined) => joined
            .expect("Codex live task failed")
            .expect("Codex live canary failed"),
        Err(_) => {
            cancel_on_timeout.cancel();
            let _settled = tokio::time::timeout(std::time::Duration::from_secs(30), task)
                .await
                .expect("Codex live cancellation did not settle");
            panic!("Codex live canary timed out");
        }
    };
    assert!(result.text.contains(CANARY), "Codex live canary mismatch");
}

#[tokio::test]
async fn live_installed_claude_ephemeral_subagent_is_explicitly_gated() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    const CANARY: &str = "R08_CLAUDE_SUBAGENT_OK";
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = heycode_runtime_claude::ClaudeRuntimeConfig::new(&workspace).unwrap();
    let runtime: Arc<dyn AgentRuntime> = Arc::new(
        heycode_runtime_claude::ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            config,
        )
        .unwrap(),
    );
    let provider = RuntimeSubagentProvider::new(
        runtime,
        Arc::new(DenyAll),
        "claude",
        "Claude delegated agent",
        sessions,
        workspace,
        2,
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let cancel_on_timeout = cancellation.clone();
    let mut task = tokio::spawn(async move {
        provider
            .start(
                request(&format!(
                    "Reply with exactly {CANARY}. Do not use tools, files, shell, network, or MCP."
                )),
                cancellation,
            )
            .await
    });
    let result = match tokio::time::timeout(std::time::Duration::from_secs(120), &mut task).await {
        Ok(joined) => joined
            .expect("Claude live task failed")
            .expect("Claude live canary failed"),
        Err(_) => {
            cancel_on_timeout.cancel();
            let _settled = tokio::time::timeout(std::time::Duration::from_secs(30), task)
                .await
                .expect("Claude live cancellation did not settle");
            panic!("Claude live canary timed out");
        }
    };
    assert!(result.text.contains(CANARY), "Claude live canary mismatch");
}

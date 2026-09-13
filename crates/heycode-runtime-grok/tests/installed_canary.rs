//! Explicitly enabled subscription canary through the official Grok process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::StreamExt as _;
use heycode_exec::{SandboxMode, SandboxService, local_subprocess_plugin, sandbox_service_plugin};
use heycode_runtime::{AccountStatus, AgentRuntimeRegistry, runtime_registry_plugin};
use heycode_runtime_grok::{GrokRuntimeConfig, grok_runtime_plugin};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn installed_subscription_account_models_and_turn_are_credential_blind() {
    if std::env::var("HEYCODE_GROK_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        grok_runtime_plugin(GrokRuntimeConfig::new(&workspace).unwrap()),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get("grok").unwrap().unwrap();
    let cancellation = CancellationToken::new();
    let cancel_on_timeout = cancellation.clone();
    let mut task = tokio::spawn(async move {
        let account = runtime.account(cancellation.clone()).await.unwrap();
        assert_eq!(account.status(), AccountStatus::Connected);
        let models = runtime.models(cancellation.clone()).await.unwrap();
        assert_eq!(models.provider.id, "grok");
        assert!(!models.models.is_empty());
        let request =
            heycode_runtime::RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
                .unwrap()
                .with_model(&models.models[0].id)
                .unwrap();
        let session = runtime.start(request, cancellation.clone()).await.unwrap();
        let mut events = session.subscribe();
        let sender = session.clone();
        let send_token = cancellation.clone();
        let sending = tokio::spawn(async move {
            sender.send(heycode_runtime::RuntimeInput::new(
                "Reply with exactly HEYCODE_GROK_SUBSCRIPTION_OK. Do not use tools or inspect files."
            ).unwrap(), send_token).await
        });
        let mut final_seen = false;
        let mut stopped = false;
        let mut tool_attempted = false;
        while let Some(event) = events.next().await {
            let event = event.unwrap();
            match event.kind() {
                heycode_runtime::RuntimeEventKind::FinalMessage { text } => {
                    final_seen = text.contains("HEYCODE_GROK_SUBSCRIPTION_OK")
                }
                heycode_runtime::RuntimeEventKind::PermissionRequested { request_id, .. } => {
                    tool_attempted = true;
                    session
                        .respond_permission(
                            heycode_runtime::RuntimePermissionResponse::new(
                                request_id.clone(),
                                heycode_runtime::RuntimePermissionDecision::Deny,
                            ),
                            cancellation.clone(),
                        )
                        .await
                        .unwrap();
                }
                heycode_runtime::RuntimeEventKind::ToolCall { .. } => tool_attempted = true,
                heycode_runtime::RuntimeEventKind::TurnFinished { reason, .. } => {
                    stopped = *reason == heycode_runtime::RuntimeFinishReason::Stop;
                    break;
                }
                _ => {}
            }
        }
        let sent = sending.await.unwrap();
        session.close(CancellationToken::new()).await.unwrap();
        assert!(sent.is_ok(), "Grok turn failed: {sent:?}");
        assert!(
            final_seen && stopped && !tool_attempted,
            "Grok canary did not complete its tool-free reply"
        );
    });
    match tokio::time::timeout(std::time::Duration::from_secs(100), &mut task).await {
        Ok(result) => result.unwrap(),
        Err(_) => {
            cancel_on_timeout.cancel();
            let _settled = tokio::time::timeout(std::time::Duration::from_secs(10), task).await;
            panic!("Grok subscription canary timed out");
        }
    }
    context.shutdown();
}

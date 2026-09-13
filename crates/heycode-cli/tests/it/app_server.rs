//! X04 production-composed stable local app-server and client.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_app_server::{
    AppCatalogRefresh, AppServerError, AppServerEvent, AppTurnReason, LocalAppClient,
    SERVICE_APP_SERVER,
};
use heycode_cli::testing::RealCompositionHarness;
use heycode_llm::testing::FakeProvider;
use heycode_llm::{FinishReason, StreamChunk};
use tokio_util::sync::CancellationToken;

async fn bounded<T>(operation: impl std::future::Future<Output = Result<T, AppServerError>>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(5), operation)
        .await
        .expect("app-server control operation must settle")
        .unwrap()
}

#[tokio::test]
async fn tui_grade_local_client_runs_the_production_host_and_streams_stable_events() {
    let provider = Arc::new(FakeProvider::new(vec![vec![
        StreamChunk::TextDelta("through-app-server".to_owned()),
        StreamChunk::Usage(heycode_core::TokenUsage {
            prompt_tokens: 7,
            completion_tokens: 3,
        }),
        StreamChunk::Finish(FinishReason::Stop),
    ]]));
    let world = RealCompositionHarness::new()
        .unwrap()
        .with_provider(provider)
        .compose()
        .unwrap();
    let server = world
        .context()
        .get::<heycode_app_server::AppServer>(SERVICE_APP_SERVER)
        .unwrap();
    let client = LocalAppClient::new(server);
    let opened = client.open().await.unwrap();
    assert_eq!(opened.runtime_id, "native");
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(32);
    let result = client
        .turn("hello", Vec::new(), events_tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.reason, AppTurnReason::Stop);
    let mut events = Vec::new();
    while let Some(notification) = events_rx.recv().await {
        events.push(notification);
    }
    assert!(matches!(
        &events[0].params.event,
        AppServerEvent::UserInput { .. }
    ));
    assert!(events.iter().any(|event| matches!(
        &event.params.event,
        AppServerEvent::AssistantDelta { text } if text == "through-app-server"
    )));
    assert!(events.iter().any(|event| matches!(
        &event.params.event,
        AppServerEvent::Usage {
            usage: heycode_core::TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 3,
            },
            ..
        }
    )));
    assert!(matches!(
        &events.last().unwrap().params.event,
        AppServerEvent::TurnFinished {
            reason: AppTurnReason::Stop,
            ..
        }
    ));
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].params.sequence == pair[0].params.sequence + 1)
    );
    client.close().await.unwrap();
    world.shutdown();
}

#[test]
fn shipping_binary_serves_stdio_v1_without_non_protocol_stdout() {
    use std::io::{BufRead as _, Write as _};
    use std::process::Stdio;

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--fake",
            "app-server",
            "--stdio-v1",
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .env("HEYCODE_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());

    let mut exchange = |operation: u64, method: &str, params: serde_json::Value| {
        let frame = serde_json::json!({
            "kind":"request",
            "operation":operation,
            "request":{"jsonrpc":"2.0","id":operation,"method":method,"params":params}
        });
        writeln!(input, "{frame}").unwrap();
        input.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
            if value["kind"] == "response" && value["operation"] == operation {
                return value;
            }
        }
    };

    let initialized = exchange(1, "initialize", serde_json::json!({}));
    assert_eq!(initialized["response"]["result"]["protocolVersion"], 1);
    let opened = exchange(2, "session/open", serde_json::json!({}));
    let session_id = opened["response"]["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let turned = exchange(
        3,
        "turn/start",
        serde_json::json!({"sessionId":session_id,"text":"stdio smoke","attachments":[]}),
    );
    assert_eq!(turned["response"]["result"]["reason"], "stop");
    let closed = exchange(
        4,
        "session/close",
        serde_json::json!({"sessionId":session_id}),
    );
    assert_eq!(closed["response"]["result"], serde_json::Value::Null);

    drop(input);
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn production_control_methods_use_live_registries_and_durable_settings() {
    let harness = RealCompositionHarness::new().unwrap();
    let workspace = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    let world = harness.compose().unwrap();
    let server = world
        .context()
        .get::<heycode_app_server::AppServer>(SERVICE_APP_SERVER)
        .unwrap();
    let client = LocalAppClient::new(server);

    let initialized = bounded(client.initialize()).await;
    assert!(initialized.capabilities.authorization);
    assert!(initialized.capabilities.models);
    assert!(initialized.capabilities.runtimes);
    assert!(initialized.capabilities.workspace);
    assert!(initialized.capabilities.mcp);
    assert!(initialized.capabilities.plugins);
    assert!(initialized.capabilities.settings);

    let authorization = bounded(client.authorization()).await;
    assert!(!authorization.is_empty());
    let authorization_wire = serde_json::to_string(&authorization).unwrap();
    assert!(!authorization_wire.contains("api_key_value"));
    assert!(!authorization_wire.contains("secret_value"));

    let providers = bounded(client.providers()).await;
    assert_eq!(providers.providers.len(), 1);
    assert_eq!(providers.current.provider, "fake");
    let models = bounded(client.models(None, AppCatalogRefresh::PreferCache)).await;
    assert_eq!(models.provider, "fake");
    assert!(
        models
            .models
            .iter()
            .any(|row| row.id == providers.current.model)
    );
    let current = models
        .models
        .iter()
        .find(|row| row.id == providers.current.model)
        .unwrap();
    if current.id != models.default_model {
        assert!(!current.selectable);
    }
    let selected = bounded(client.select_model(&models.default_model)).await;
    assert_eq!(selected.model, models.default_model);

    let runtimes = bounded(client.runtimes()).await;
    assert_eq!(runtimes.current.runtime, "native");
    assert_eq!(
        runtimes
            .runtimes
            .iter()
            .map(|runtime| runtime.id.as_str())
            .collect::<Vec<_>>(),
        [
            "claude",
            "codex",
            "deepseek-harness",
            "grok",
            "native",
            "opencode"
        ]
    );
    let native = runtimes
        .runtimes
        .iter()
        .find(|runtime| runtime.id == "native")
        .unwrap();
    assert_eq!(native.kind, "native");
    assert_eq!(
        native.workspace,
        heycode_app_server::AppRuntimeWorkspace::Composed
    );
    let selected_runtime = bounded(client.select_runtime("native")).await;
    assert_eq!(selected_runtime.runtime, "native");
    let selected_workspace = bounded(client.select_workspace(&workspace)).await;
    assert_eq!(selected_workspace.cwd, workspace);
    assert_eq!(selected_workspace.runtime, "native");
    assert!(!selected_workspace.selected);

    let settings = bounded(client.settings()).await;
    let routing = settings
        .iter()
        .find(|row| row.namespace == "routing")
        .unwrap();
    assert!(routing.exposed);
    // S15: a client receives values only through the verified redacted
    // projection. `exposed` now means exposure was PROVED, so an exposed row
    // must carry projected values, and an unexposed one must carry none at all.
    assert!(
        routing.resolved.is_some(),
        "a proved namespace projects values"
    );
    for row in &settings {
        if !row.exposed {
            assert!(
                row.schema.is_none()
                    && row.defaults.is_none()
                    && row.base.is_none()
                    && row.user.is_none()
                    && row.project.is_none()
                    && row.managed.is_none()
                    && row.resolved.is_none(),
                "namespace `{}` was not proved safe and must project no value",
                row.namespace
            );
        }
    }
    let committed = bounded(client.replace_setting(
        "routing",
        serde_json::json!({
            "runtime": selected.runtime,
            "provider": selected.provider,
            "model": selected.model,
        }),
        routing.revision,
    ))
    .await;
    assert_eq!(committed.revision, routing.revision + 1);

    let mcp = bounded(client.mcp()).await;
    assert_eq!(mcp["active"], true);
    let plugins = bounded(client.plugins()).await;
    assert!(
        plugins
            .plugins
            .iter()
            .any(|row| row.id == "app-server-controls")
    );
    assert!(
        plugins
            .contributions
            .iter()
            .any(|row| { row.kind == "app_server_method" && row.name == "settings/replace" })
    );

    world.shutdown();
}

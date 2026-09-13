//! X06 typed Rust SDK start/resume/stream/cancel contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_sdk::{
    APP_SERVER_PROTOCOL_VERSION, AppClient, AppServerError, AppServerEvent, AppTransport,
    AppTurnReason,
};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

fn configuration_capabilities() -> Value {
    serde_json::json!({
        "systemPrompt":"supported",
        "tools":"supported",
        "model":"supported",
        "reasoningEffort":"supported"
    })
}

fn session_info(session_id: &str, runtime_id: &str, configuration: Value) -> Value {
    let model_configuration = configuration
        .get("model")
        .and_then(Value::as_str)
        .map(|model| {
            serde_json::json!({
                "model":model,
                "displayName":"Runtime Model",
                "resolvedModel":"runtime-model-2026",
                "description":"Provider supplied description",
                "contextWindow":200000,
                "defaultReasoningEffort":"medium",
                "reasoningEfforts":["low","medium","high"]
            })
        });
    serde_json::json!({
        "sessionId":session_id,
        "runtimeId":runtime_id,
        "cwd":"/workspace",
        "configuration":configuration,
        "configurationCapabilities":configuration_capabilities(),
        "modelConfiguration":model_configuration
    })
}

#[test]
fn older_session_info_defaults_additive_runtime_configuration_fields() {
    let info: heycode_sdk::AppSessionInfo = serde_json::from_value(serde_json::json!({
        "sessionId":"legacy-session",
        "runtimeId":"legacy-runtime",
        "cwd":"/workspace"
    }))
    .unwrap();
    assert_eq!(
        info.configuration,
        heycode_sdk::AppRuntimeConfiguration::default()
    );
    assert_eq!(
        info.configuration_capabilities,
        heycode_sdk::AppRuntimeConfigurationCapabilities::default()
    );
    assert_eq!(info.model_configuration, None);
}

#[derive(Default)]
struct ScriptedTransport {
    cancel: Notify,
}

#[async_trait]
impl AppTransport for ScriptedTransport {
    async fn exchange(
        &self,
        request: String,
        notifications: mpsc::Sender<String>,
        _cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let id = request["id"].clone();
        let result = match request["method"].as_str().unwrap() {
            "initialize" => serde_json::json!({
                "protocolVersion":APP_SERVER_PROTOCOL_VERSION,
                "server":{"name":"heycode","version":"0.1.0"},
                "capabilities":{
                    "turns":true,"attachments":true,"cancel":true,
                    "authorization":true,"models":true,"mcp":true,
                    "plugins":true,"settings":true
                }
            }),
            "session/open" => session_info(
                "session-1",
                "native",
                request["params"]["configuration"].clone(),
            ),
            "session/configure" => session_info(
                "session-1",
                "native",
                request["params"]["configuration"].clone(),
            ),
            "runtime/models" => serde_json::json!([{
                "model":"runtime-model",
                "displayName":"Runtime Model",
                "resolvedModel":"runtime-model-2026",
                "description":"Provider supplied description",
                "contextWindow":200000,
                "defaultReasoningEffort":"medium",
                "reasoningEfforts":["low","medium","high"]
            }]),
            "turn/start" => {
                notifications
                    .send(
                        serde_json::json!({
                            "jsonrpc":"2.0","method":"session/event",
                            "params":{"sessionId":"session-1","sequence":40,
                                "event":{"type":"turn_started","turn_id":"7"}}
                        })
                        .to_string(),
                    )
                    .await
                    .unwrap();
                self.cancel.notified().await;
                notifications
                    .send(
                        serde_json::json!({
                            "jsonrpc":"2.0","method":"session/event",
                            "params":{"sessionId":"session-1","sequence":41,
                                "event":{"type":"turn_finished","turn_id":"7",
                                    "reason":"cancelled"}}
                        })
                        .to_string(),
                    )
                    .await
                    .unwrap();
                serde_json::json!({"turnId":"7","reason":"cancelled"})
            }
            "turn/cancel" => {
                self.cancel.notify_waiters();
                serde_json::Value::Null
            }
            method => panic!("unexpected method {method}"),
        };
        Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
    }
}

#[tokio::test]
async fn typed_configuration_and_runtime_model_choices_round_trip() {
    let client = AppClient::new(Arc::new(ScriptedTransport::default()));
    let configuration = heycode_sdk::AppRuntimeConfiguration {
        system_prompt: Some("Use exact instructions".to_owned()),
        tools: Some(vec![heycode_core::ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        }]),
        model: Some("runtime-model".to_owned()),
        reasoning_effort: Some("high".to_owned()),
    };
    let debug = format!("{configuration:?}");
    assert!(!debug.contains("Use exact instructions"));
    assert!(!debug.contains("runtime-model"));
    assert!(!debug.contains("high"));
    assert!(!debug.contains("read_file"));
    let opened = client
        .start_with_configuration(configuration.clone())
        .await
        .unwrap();
    assert_eq!(opened.configuration, configuration);
    let metadata = opened.model_configuration.unwrap();
    assert_eq!(metadata.model, "runtime-model");
    assert_eq!(metadata.display_name, "Runtime Model");
    assert_eq!(
        metadata.resolved_model.as_deref(),
        Some("runtime-model-2026")
    );
    assert_eq!(metadata.context_window, Some(200_000));
    assert_eq!(
        opened.configuration_capabilities.tools,
        heycode_sdk::AppCapabilityEvidence::Supported
    );

    let disabled = client
        .configure(heycode_sdk::AppRuntimeConfiguration {
            tools: Some(Vec::new()),
            ..heycode_sdk::AppRuntimeConfiguration::default()
        })
        .await
        .unwrap();
    assert_eq!(disabled.configuration.tools, Some(Vec::new()));
    assert_eq!(
        client.runtime_models().await.unwrap(),
        [heycode_sdk::AppRuntimeModelConfiguration {
            model: "runtime-model".to_owned(),
            display_name: "Runtime Model".to_owned(),
            resolved_model: Some("runtime-model-2026".to_owned()),
            description: Some("Provider supplied description".to_owned()),
            context_window: Some(200_000),
            default_reasoning_effort: Some("medium".to_owned()),
            reasoning_efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        }]
    );
}

#[tokio::test]
async fn typed_client_starts_resumes_streams_and_cancels_one_owned_turn() {
    let client = AppClient::new(Arc::new(ScriptedTransport::default()));
    let started = client.start().await.unwrap();
    assert_eq!(started.session_id, "session-1");
    let resumed = client.resume("session-1").await.unwrap();
    assert_eq!(resumed, started);

    let (events_tx, mut events_rx) = mpsc::channel(8);
    let turn = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .turn("wait", Vec::new(), events_tx, CancellationToken::new())
                .await
        })
    };
    let started_event = events_rx.recv().await.unwrap();
    assert!(matches!(
        started_event.params.event,
        AppServerEvent::TurnStarted { ref turn_id } if turn_id == "7"
    ));
    client.cancel().await.unwrap();
    let finished_event = events_rx.recv().await.unwrap();
    assert!(matches!(
        finished_event.params.event,
        AppServerEvent::TurnFinished {
            reason: AppTurnReason::Cancelled,
            ..
        }
    ));
    assert_eq!(finished_event.params.sequence, 41);
    assert_eq!(
        turn.await.unwrap().unwrap().reason,
        AppTurnReason::Cancelled
    );
}

struct SelectionTransport;

#[async_trait]
impl AppTransport for SelectionTransport {
    async fn exchange(
        &self,
        request: String,
        _notifications: mpsc::Sender<String>,
        _cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let id = request["id"].clone();
        let result = match request["method"].as_str().unwrap() {
            "initialize" => serde_json::json!({
                "protocolVersion":APP_SERVER_PROTOCOL_VERSION,
                "server":{"name":"heycode","version":"0.1.0"},
                "capabilities":{
                    "turns":true,"attachments":true,"cancel":true,
                    "authorization":true,"models":true,"runtimes":true,
                    "workspace":true,"mcp":true,"plugins":true,"settings":true
                }
            }),
            "runtimes/list" => serde_json::json!({
                "current":{"runtime":"native","provider":"alpha","model":"model-a","effort":null},
                "runtimes":[{
                    "id":"native","displayName":"Native","kind":"native",
                    "workspace":"composed","capabilities":{
                        "models":"supported","resume":"supported","fork":"unsupported",
                        "steer":"unsupported","followUp":"unsupported",
                        "permissions":"unsupported","questions":"unsupported",
                        "compaction":"supported"
                    },"configuration":{
                        "systemPrompt":"unsupported","tools":"supported",
                        "model":"supported","reasoningEffort":"supported"
                    }
                },{
                    "id":"delegated","displayName":"Delegated","kind":"delegated",
                    "workspace":"selectable","capabilities":{
                        "models":"unknown","resume":"supported","fork":"supported",
                        "steer":"supported","followUp":"unknown",
                        "permissions":"supported","questions":"supported",
                        "compaction":"supported"
                    }
                }]
            }),
            "runtime/select" => serde_json::json!({
                "runtime":request["params"]["runtime"],
                "provider":"alpha","model":"model-a","effort":null
            }),
            "workspace/select" if request["params"]["cwd"] == "/denied" => {
                return Ok(serde_json::json!({
                    "jsonrpc":"2.0","id":id,
                    "error":{"code":-32003,"message":"app-server selection is known but not installed here"}
                })
                .to_string());
            }
            "workspace/select" => serde_json::json!({
                "cwd":request["params"]["cwd"],"runtime":"delegated","selected":true
            }),
            method => panic!("unexpected method {method}"),
        };
        Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
    }
}

#[tokio::test]
async fn typed_runtime_and_workspace_controls_preserve_closed_rows_and_error_classes() {
    let client = AppClient::new(Arc::new(SelectionTransport));
    let initialized = client.initialize().await.unwrap();
    assert!(initialized.capabilities.runtimes);
    assert!(initialized.capabilities.workspace);

    let catalog = client.runtimes().await.unwrap();
    assert_eq!(catalog.runtimes.len(), 2);
    assert_eq!(
        catalog.runtimes[0].workspace,
        heycode_sdk::AppRuntimeWorkspace::Composed
    );
    assert_eq!(
        catalog.runtimes[1].workspace,
        heycode_sdk::AppRuntimeWorkspace::Selectable
    );
    assert_eq!(
        catalog.runtimes[1].capabilities.follow_up,
        heycode_sdk::AppCapabilityEvidence::Unknown
    );
    assert_eq!(
        catalog.runtimes[1].configuration,
        heycode_sdk::AppRuntimeConfigurationCapabilities::default()
    );

    let route = client.select_runtime("delegated").await.unwrap();
    assert_eq!(route.runtime, "delegated");
    let workspace = client
        .select_workspace(std::path::Path::new("/workspace/nested"))
        .await
        .unwrap();
    assert!(workspace.selected);
    assert_eq!(workspace.runtime, "delegated");
    let error = client
        .select_workspace(std::path::Path::new("/denied"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), heycode_sdk::AppServerErrorCode::Unsupported);
}

struct InvalidSequenceTransport;

#[async_trait]
impl AppTransport for InvalidSequenceTransport {
    async fn exchange(
        &self,
        request: String,
        notifications: mpsc::Sender<String>,
        _cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let id = request["id"].clone();
        if request["method"] == "initialize" {
            return Ok(serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":1,"server":{"name":"heycode","version":"0.1.0"},
                    "capabilities":{"turns":true,"attachments":true,"cancel":true,
                        "authorization":false,"models":false,"mcp":false,
                        "plugins":false,"settings":false}
                }
            })
            .to_string());
        }
        if request["method"] == "session/open" {
            return Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":
                session_info("session-1", "native", request["params"]["configuration"].clone())
            })
            .to_string());
        }
        for sequence in [4_u64, 6] {
            notifications
                .send(
                    serde_json::json!({
                        "jsonrpc":"2.0","method":"session/event",
                        "params":{"sessionId":"session-1","sequence":sequence,
                            "event":{"type":"notice","code":"test","message":"safe"}}
                    })
                    .to_string(),
                )
                .await
                .unwrap();
        }
        Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
            "turnId":"1","reason":"stop"
        }})
        .to_string())
    }
}

#[tokio::test]
async fn client_rejects_notification_sequence_gaps_before_returning_success() {
    let client = AppClient::new(Arc::new(InvalidSequenceTransport));
    client.start().await.unwrap();
    let (events, _receiver) = mpsc::channel(8);
    let error = client
        .turn("hello", Vec::new(), events, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        heycode_sdk::AppServerErrorCode::InvalidRequest
    );
}

#[derive(Default)]
struct IdeFixtureTransport {
    answered: AtomicBool,
    answer_count: AtomicUsize,
    permission_answered: Notify,
}

#[async_trait]
impl AppTransport for IdeFixtureTransport {
    async fn exchange(
        &self,
        request: String,
        notifications: mpsc::Sender<String>,
        cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let id = request["id"].clone();
        let result =
            match request["method"].as_str().unwrap() {
                "initialize" => serde_json::json!({
                    "protocolVersion":APP_SERVER_PROTOCOL_VERSION,
                    "server":{"name":"heycode","version":"0.1.0"},
                    "capabilities":{
                        "turns":true,"attachments":true,"cancel":true,
                        "authorization":false,"models":false,"runtimes":true,
                        "workspace":true,"mcp":false,"plugins":false,"settings":false
                    }
                }),
                "session/open" => session_info(
                    "ide-session-1",
                    "opencode",
                    request["params"]["configuration"].clone(),
                ),
                "session/permission/respond" => {
                    assert_eq!(request["params"]["sessionId"], "ide-session-1");
                    assert_eq!(request["params"]["requestId"], "permission-7");
                    assert_eq!(request["params"]["decision"], "allow_once");
                    if self
                        .answered
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        return Ok(serde_json::json!({
                            "jsonrpc":"2.0","id":id,
                            "error":{"code":-32001,"message":"request already settled"}
                        })
                        .to_string());
                    }
                    self.answer_count.fetch_add(1, Ordering::SeqCst);
                    self.permission_answered.notify_waiters();
                    Value::Null
                }
                "turn/start" => {
                    assert_eq!(request["params"]["sessionId"], "ide-session-1");
                    notifications
                    .send(serde_json::json!({
                        "jsonrpc":"2.0","method":"session/event",
                        "params":{"sessionId":"ide-session-1","sequence":40,
                            "event":{"type":"permission_requested","request_id":"permission-7",
                                "action":"Run fixture","detail":"IDE confirmation required"}}
                    }).to_string())
                    .await
                    .unwrap();
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return Err(AppServerError::cancelled()),
                        () = self.permission_answered.notified() => {}
                    }
                    notifications
                        .send(
                            serde_json::json!({
                                "jsonrpc":"2.0","method":"session/event",
                                "params":{"sessionId":"ide-session-1","sequence":41,
                                    "event":{"type":"assistant_delta","text":"approved"}}
                            })
                            .to_string(),
                        )
                        .await
                        .unwrap();
                    notifications
                        .send(
                            serde_json::json!({
                                "jsonrpc":"2.0","method":"session/event",
                                "params":{"sessionId":"ide-session-1","sequence":42,
                                    "event":{"type":"turn_finished","turn_id":"turn-1",
                                        "reason":"stop"}}
                            })
                            .to_string(),
                        )
                        .await
                        .unwrap();
                    serde_json::json!({"turnId":"turn-1","reason":"stop"})
                }
                method => panic!("unexpected IDE fixture method {method}"),
            };
        Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
    }
}

#[tokio::test]
async fn vscode_style_transport_uses_one_host_session_and_correlated_permission_loop() {
    let transport = Arc::new(IdeFixtureTransport::default());
    let client = AppClient::new(Arc::clone(&transport));
    let session = client.start().await.unwrap();
    assert_eq!(session.session_id, "ide-session-1");
    assert_eq!(session.runtime_id, "opencode");

    let (event_tx, mut event_rx) = mpsc::channel(8);
    let turn_client = client.clone();
    let turn = tokio::spawn(async move {
        turn_client
            .turn(
                "exercise IDE transport",
                Vec::new(),
                event_tx,
                CancellationToken::new(),
            )
            .await
    });
    let permission = event_rx.recv().await.unwrap();
    assert_eq!(
        permission.params.session_id.as_deref(),
        Some("ide-session-1")
    );
    assert!(matches!(
        permission.params.event,
        AppServerEvent::PermissionRequested { ref request_id, .. }
            if request_id == "permission-7"
    ));

    client
        .respond_permission(
            "permission-7",
            heycode_sdk::AppPermissionDecision::AllowOnce,
        )
        .await
        .unwrap();
    let duplicate = client
        .respond_permission(
            "permission-7",
            heycode_sdk::AppPermissionDecision::AllowOnce,
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), heycode_sdk::AppServerErrorCode::Conflict);

    let result = turn.await.unwrap().unwrap();
    assert_eq!(result.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(result.reason, AppTurnReason::Stop);
    let assistant = event_rx.recv().await.unwrap();
    let settled = event_rx.recv().await.unwrap();
    assert!(
        matches!(assistant.params.event, AppServerEvent::AssistantDelta { ref text } if text == "approved")
    );
    assert!(
        matches!(settled.params.event, AppServerEvent::TurnFinished { ref turn_id, .. } if turn_id == "turn-1")
    );
    assert_eq!(transport.answer_count.load(Ordering::SeqCst), 1);
}

#[test]
fn shared_v1_fixture_round_trips_baseline_events_and_control_shapes() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../sdks/fixtures/app-server-v1.json")).unwrap();
    assert_eq!(fixture["protocolVersion"], APP_SERVER_PROTOCOL_VERSION);
    let initialized: heycode_sdk::AppInitializeResult =
        serde_json::from_value(fixture["initialize"].clone()).unwrap();
    assert!(initialized.capabilities.settings);
    let session: heycode_sdk::AppSessionInfo =
        serde_json::from_value(fixture["session"].clone()).unwrap();
    assert_eq!(session.session_id, "session-1");
    let turn: heycode_sdk::AppTurnResult = serde_json::from_value(fixture["turn"].clone()).unwrap();
    assert_eq!(turn.reason, AppTurnReason::Stop);
    let notifications = fixture["notifications"].as_array().unwrap();
    let mut decoded_event_types = Vec::new();
    for raw in notifications {
        let typed: heycode_sdk::AppServerNotification =
            serde_json::from_value(raw.clone()).unwrap();
        let round_trip: heycode_sdk::AppServerNotification =
            serde_json::from_slice(&serde_json::to_vec(&typed).unwrap()).unwrap();
        assert_eq!(round_trip, typed);
        decoded_event_types.push(
            serde_json::to_value(&typed.params.event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let usage = notifications
        .iter()
        .find_map(|raw| {
            match serde_json::from_value::<heycode_sdk::AppServerNotification>(raw.clone())
                .unwrap()
                .params
                .event
            {
                heycode_sdk::AppServerEvent::Usage { context, .. } => context,
                _ => None,
            }
        })
        .expect("fixture carries exact delegated-runtime context evidence");
    assert_eq!(usage.tokens, 2_767);
    assert_eq!(usage.context_window, 1_000_000);
    // Pins the same property as the TypeScript twin in
    // sdks/typescript/test/client.test.mjs: the exact ordered sequence of
    // closed event type names, so a renamed or reordered event reddens both
    // SDKs rather than only the npm package.
    assert_eq!(
        decoded_event_types,
        [
            "user_input",
            "turn_started",
            "assistant_delta",
            "reasoning_delta",
            "tool_started",
            "tool_finished",
            "usage",
            "plan_changed",
            "notice",
            "authorization_prompt_requested",
            "authorization_prompt_resolved",
            "permission_requested",
            "question_requested",
            "assistant_audio",
            "turn_finished",
        ]
    );
    let authorization: heycode_sdk::AppAuthorizationFlow =
        serde_json::from_value(fixture["authorization"].clone()).unwrap();
    assert!(!authorization.credential.inspected);
    let settings: heycode_sdk::AppSettingsSnapshot =
        serde_json::from_value(fixture["settings"].clone()).unwrap();
    assert!(settings.exposed);
}

#[test]
fn audio_event_round_trips_metadata_without_a_raw_byte_field() {
    let attachment = heycode_core::AttachmentMetadata::new_audio(
        heycode_core::AttachmentContentId::from_sha256([0x71; 32]),
        heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
        16_044,
        Some("answer.wav".to_owned()),
        heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
    )
    .unwrap();
    let event = AppServerEvent::AssistantAudio {
        attachments: vec![attachment.clone()],
    };
    let wire = serde_json::to_string(&event).unwrap();
    assert!(wire.contains("assistant_audio"));
    assert!(wire.contains("duration_ms"));
    assert!(!wire.contains("RAW-AUDIO"));
    assert!(!wire.contains("base64"));
    assert_eq!(
        serde_json::from_str::<AppServerEvent>(&wire).unwrap(),
        event
    );
    let mut hostile = serde_json::from_str::<serde_json::Value>(&wire).unwrap();
    hostile["attachments"][0]["data"] = serde_json::json!("UkFXLUFVRElP");
    assert!(serde_json::from_value::<AppServerEvent>(hostile).is_err());
    let AppServerEvent::AssistantAudio { attachments } = event else {
        panic!("expected audio event")
    };
    assert_eq!(attachments, [attachment]);
}

/// A classified error may carry a safe one-line detail that the message alone
/// cannot express ("Codex CLI is unavailable"), and the detail survives the
/// JSON-RPC wire as `error.data.detail`.
#[test]
fn error_detail_is_displayed_and_rejects_unsafe_text() {
    let error = AppServerError::unavailable()
        .with_detail("Codex CLI is unavailable")
        .expect("a short control-free detail is accepted");
    assert_eq!(error.detail(), Some("Codex CLI is unavailable"));
    assert_eq!(
        error.to_string(),
        "app-server is unavailable: Codex CLI is unavailable"
    );
    assert!(
        AppServerError::unavailable()
            .with_detail("has\u{7}bell")
            .is_err(),
        "control characters never become a detail"
    );
    assert_eq!(
        AppServerError::unavailable().to_string(),
        "app-server is unavailable"
    );
}

struct DetailedErrorTransport;

#[async_trait]
impl AppTransport for DetailedErrorTransport {
    async fn exchange(
        &self,
        request: String,
        _notifications: mpsc::Sender<String>,
        _cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let id = request["id"].clone();
        if request["method"] == "initialize" {
            return Ok(serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":1,"server":{"name":"heycode","version":"0.1.0"},
                    "capabilities":{"turns":true,"attachments":true,"cancel":true,
                        "authorization":false,"models":false,"mcp":false,
                        "plugins":false,"settings":false}
                }
            })
            .to_string());
        }
        Ok(serde_json::json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32002,"message":"app-server is unavailable",
                     "data":{"detail":"Codex CLI is unavailable"}}
        })
        .to_string())
    }
}

#[tokio::test]
async fn error_detail_survives_the_wire() {
    let client = AppClient::new(Arc::new(DetailedErrorTransport));
    client.initialize().await.unwrap();
    let error = client.open().await.unwrap_err();
    assert_eq!(error.code(), heycode_sdk::AppServerErrorCode::Unavailable);
    assert_eq!(error.detail(), Some("Codex CLI is unavailable"));
}

#[tokio::test]
async fn selected_question_answers_preserve_labels_and_correlation_on_wire() {
    #[derive(Default)]
    struct QuestionTransport {
        base: ScriptedTransport,
        replies: std::sync::Mutex<Vec<Value>>,
    }
    #[async_trait]
    impl AppTransport for QuestionTransport {
        async fn exchange(
            &self,
            request: String,
            notifications: mpsc::Sender<String>,
            cancellation: CancellationToken,
        ) -> Result<String, AppServerError> {
            let parsed: Value = serde_json::from_str(&request).unwrap();
            if parsed["method"] == "session/question/respond" {
                self.replies.lock().unwrap().push(parsed["params"].clone());
                return Ok(
                    serde_json::json!({"jsonrpc":"2.0","id":parsed["id"],"result":{}}).to_string(),
                );
            }
            self.base
                .exchange(request, notifications, cancellation)
                .await
        }
    }
    let transport = Arc::new(QuestionTransport::default());
    let client = AppClient::new(transport.clone());
    client.start().await.unwrap();
    client
        .respond_question_selected("question-2", &["Backend, API".into(), "UI".into()])
        .await
        .unwrap();
    client
        .respond_question("question-3", "[custom JSON-looking text]")
        .await
        .unwrap();
    let replies = transport.replies.lock().unwrap();
    assert_eq!(
        replies[0],
        serde_json::json!({"sessionId":"session-1","requestId":"question-2","selectedAnswers":["Backend, API","UI"]})
    );
    assert_eq!(replies[1]["answer"], "[custom JSON-looking text]");
    assert!(replies[1].get("selectedAnswers").is_none());
}

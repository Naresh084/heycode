//! R10/R11 ACP process, framing, catalog, event and permission fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt as _;
use heycode_llm::CapabilitySupport;
use heycode_runtime::{
    AcpFrameDecoder, AcpProcess, AcpProcessFactory, AcpProcessSpec, AgentRuntime,
    RuntimeConfiguration, RuntimeError, RuntimeErrorCode, RuntimeEventKind, RuntimeInput,
    RuntimePermissionDecision, RuntimePermissionResponse, RuntimeRequestId, RuntimeResume,
    RuntimeSessionId, RuntimeStart, opencode_acp_runtime,
};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

#[test]
fn acp_ndjson_decoder_is_fragmentation_exact_and_fails_closed() {
    let first = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\r\n";
    let second = b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}\n";
    let joined = [first.as_slice(), second.as_slice()].concat();

    for split in 0..=joined.len() {
        let mut decoder = AcpFrameDecoder::new(1024).unwrap();
        let mut frames = decoder.push(&joined[..split]).unwrap();
        frames.extend(decoder.push(&joined[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(frames.len(), 2, "split {split}");
        assert_eq!(frames[0]["id"], 1, "split {split}");
        assert_eq!(frames[1]["method"], "session/update", "split {split}");
    }

    let mut invalid_utf8 = AcpFrameDecoder::new(32).unwrap();
    assert!(invalid_utf8.push(&[0xff, b'\n']).is_err());
    let mut oversized = AcpFrameDecoder::new(8).unwrap();
    assert!(oversized.push(b"123456789").is_err());
    let mut torn = AcpFrameDecoder::new(32).unwrap();
    assert!(torn.push(br#"{"id":1}"#).unwrap().is_empty());
    assert!(torn.finish().is_err());
}

#[derive(Clone, Copy)]
enum FixtureMode {
    Permission,
    ConfigMissing,
    ConfigNotApplied,
    DependentEffort,
    WrongInitializeId,
    AuthReady,
    AuthAbsent,
    AuthRejected,
    LegacyModels,
}

#[derive(Default)]
struct FixtureFacts {
    specs: Mutex<Vec<FixtureSpec>>,
    requests: Mutex<Vec<Value>>,
    lifecycles: Mutex<Vec<CancellationToken>>,
    permission_answers: Mutex<Vec<Value>>,
    selected_models: Mutex<Vec<String>>,
    close_calls: AtomicUsize,
    cancel_notifications: AtomicUsize,
}

#[derive(Debug, Clone)]
struct FixtureSpec {
    program: std::path::PathBuf,
    cwd: std::path::PathBuf,
    args: Vec<OsString>,
    environment_names: Vec<OsString>,
}

struct FixtureFactory {
    facts: Arc<FixtureFacts>,
    mode: FixtureMode,
}

#[async_trait]
impl AcpProcessFactory for FixtureFactory {
    async fn spawn(
        &self,
        spec: AcpProcessSpec,
        lifecycle: CancellationToken,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn AcpProcess>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.facts.specs.lock().unwrap().push(FixtureSpec {
            program: spec.program().to_path_buf(),
            cwd: spec.cwd().to_path_buf(),
            args: spec.args().to_vec(),
            environment_names: spec
                .environment()
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        });
        self.facts.lifecycles.lock().unwrap().push(lifecycle);
        Ok(FixtureProcess::new(Arc::clone(&self.facts), self.mode))
    }
}

struct FixtureState {
    prompt_id: Option<Value>,
    permission_sent: bool,
    model: String,
    effort: String,
}

struct FixtureProcess {
    facts: Arc<FixtureFacts>,
    mode: FixtureMode,
    sender: mpsc::UnboundedSender<Option<Vec<u8>>>,
    receiver: AsyncMutex<mpsc::UnboundedReceiver<Option<Vec<u8>>>>,
    state: Mutex<FixtureState>,
    closed: AtomicBool,
}

impl FixtureProcess {
    fn new(facts: Arc<FixtureFacts>, mode: FixtureMode) -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            facts,
            mode,
            sender,
            receiver: AsyncMutex::new(receiver),
            state: Mutex::new(FixtureState {
                prompt_id: None,
                permission_sent: false,
                model: "opencode-go/glm-5.3-flash".to_owned(),
                effort: "medium".to_owned(),
            }),
            closed: AtomicBool::new(false),
        })
    }

    fn enqueue(&self, value: Value) {
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        let one = bytes.len().min(1);
        let two = bytes.len().min(7);
        for chunk in [&bytes[..one], &bytes[one..two], &bytes[two..]] {
            if !chunk.is_empty() {
                self.sender.send(Some(chunk.to_vec())).unwrap();
            }
        }
    }

    fn initialize(&self, id: Value) {
        let id = match self.mode {
            FixtureMode::Permission
            | FixtureMode::ConfigMissing
            | FixtureMode::ConfigNotApplied
            | FixtureMode::DependentEffort
            | FixtureMode::AuthReady
            | FixtureMode::AuthAbsent
            | FixtureMode::AuthRejected
            | FixtureMode::LegacyModels => id,
            FixtureMode::WrongInitializeId => serde_json::json!(999_999),
        };
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":1,
                "agentCapabilities":{
                    "loadSession":true,
                    "promptCapabilities":{"image":false,"audio":false,"embeddedContext":false},
                    "mcpCapabilities":{"http":false,"sse":false},
                    "sessionCapabilities":{"close":{},"resume":{}}
                },
                "authMethods": if matches!(self.mode, FixtureMode::AuthReady | FixtureMode::AuthRejected) {
                    serde_json::json!([{"id":"cached_token","name":"Signed-in account"}])
                } else { serde_json::json!([]) },
                "agentInfo":{"name":"Fixture ACP","version":"1.0.0"}
            }
        }));
    }

    fn session_response(&self, id: Value) {
        if matches!(self.mode, FixtureMode::LegacyModels) {
            self.enqueue(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                "sessionId":"fixture-session", "models": {
                    "currentModelId":"grok-4.6", "availableModels":[
                        {"modelId":"grok-4.6","name":"Grok 4.6"},
                        {"modelId":"grok-4.5","name":"Grok 4.5"}
                    ]
                }
            }}));
            return;
        }
        self.config_options_response(id);
    }

    fn config_options_response(&self, id: Value) {
        let state = self.state.lock().unwrap();
        let (effort_id, effort_options) = if matches!(self.mode, FixtureMode::DependentEffort)
            && state.model == "openrouter/z-ai/glm-5.3"
        {
            (
                "thought-next",
                serde_json::json!([
                    {"value":"low","name":"Low"},
                    {"value":"high","name":"High"}
                ]),
            )
        } else {
            (
                "thought",
                serde_json::json!([
                    {"value":"low","name":"Low"},
                    {"value":"medium","name":"Medium"},
                    {"value":"high","name":"High"}
                ]),
            )
        };
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","id":id,"result":{
                "sessionId":"fixture-session",
                "configOptions":[{
                    "id":"model","name":"Model","category":"model","type":"select",
                    "currentValue":state.model,
                    "options":[
                        {"value":"opencode-go/glm-5.3-flash","name":"GLM-5.3-Flash"},
                        {"value":"openrouter/z-ai/glm-5.3","name":"GLM-5.3"}
                    ]
                },{
                    "id":effort_id,"name":"Thought level","category":"thought_level",
                    "type":"select","currentValue":state.effort,
                    "options":effort_options
                }]
            }
        }));
    }

    fn begin_prompt(&self, id: Value) {
        let mut state = self.state.lock().unwrap();
        state.prompt_id = Some(id);
        state.permission_sent = true;
        drop(state);
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"fixture-session","update":{
                    "sessionUpdate":"agent_thought_chunk",
                    "content":{"type":"text","text":"checking"}
                }
            }
        }));
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"fixture-session","update":{
                    "sessionUpdate":"tool_call","toolCallId":"tool-1",
                    "title":"Fixture tool","kind":"execute","status":"pending",
                    "rawInput":{"command":"fixture"}
                }
            }
        }));
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","id":"permission-1","method":"session/request_permission",
            "params":{
                "sessionId":"fixture-session",
                "toolCall":{"toolCallId":"tool-1","title":"Fixture tool","status":"pending"},
                "options":[
                    {"optionId":"once","name":"Allow once","kind":"allow_once"},
                    {"optionId":"always","name":"Always allow","kind":"allow_always"},
                    {"optionId":"reject","name":"Reject","kind":"reject_once"}
                ]
            }
        }));
    }

    fn finish_permission_prompt(&self, response: Value) {
        self.facts.permission_answers.lock().unwrap().push(response);
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"fixture-session","update":{
                    "sessionUpdate":"tool_call_update","toolCallId":"tool-1",
                    "status":"completed","rawOutput":{"ok":true}
                }
            }
        }));
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"fixture-session","update":{
                    "sessionUpdate":"agent_message_chunk",
                    "content":{"type":"text","text":"done"}
                }
            }
        }));
        let prompt_id = self.state.lock().unwrap().prompt_id.take().unwrap();
        self.enqueue(serde_json::json!({
            "jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"end_turn"}
        }));
    }
}

#[tokio::test]
async fn opencode_applies_exact_model_and_thought_level_initially_and_between_turns() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::Permission, Arc::clone(&facts), &workspace);
    let descriptor = runtime.descriptor().configuration_capabilities();
    assert_eq!(descriptor.system_prompt, CapabilitySupport::Unsupported);
    assert_eq!(descriptor.tools, CapabilitySupport::Unsupported);
    assert_eq!(descriptor.model, CapabilitySupport::Supported);
    assert_eq!(descriptor.reasoning_effort, CapabilitySupport::Supported);

    let models = runtime
        .model_configurations(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(
        models[0].default_reasoning_effort.as_deref(),
        Some("medium")
    );
    assert_eq!(models[0].reasoning_efforts, ["low", "medium", "high"]);
    assert_eq!(
        models[1].default_reasoning_effort.as_deref(),
        Some("medium")
    );
    assert_eq!(models[1].reasoning_efforts, ["low", "medium", "high"]);
    facts.requests.lock().unwrap().clear();

    let initial = RuntimeConfiguration::new()
        .with_model("opencode-go/glm-5.3-flash")
        .unwrap()
        .with_reasoning_effort("high")
        .unwrap();
    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
        .unwrap()
        .with_configuration(initial);
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    let effective = session
        .configure(
            RuntimeConfiguration::new()
                .with_model("openrouter/z-ai/glm-5.3")
                .unwrap()
                .with_reasoning_effort("low")
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(effective.model(), Some("openrouter/z-ai/glm-5.3"));
    assert_eq!(effective.reasoning_effort(), Some("low"));

    let selections = facts
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "session/set_config_option")
        .map(|request| {
            (
                request["params"]["configId"].as_str().unwrap().to_owned(),
                request["params"]["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selections,
        [
            ("model".to_owned(), "opencode-go/glm-5.3-flash".to_owned()),
            ("thought".to_owned(), "high".to_owned()),
            ("model".to_owned(), "openrouter/z-ai/glm-5.3".to_owned()),
            ("thought".to_owned(), "low".to_owned()),
        ]
    );
    session.close(CancellationToken::new()).await.unwrap();
}

#[tokio::test]
async fn acp_re_resolves_model_dependent_effort_controls() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::DependentEffort, Arc::clone(&facts), &workspace);
    let models = runtime
        .model_configurations(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(
        models[0].default_reasoning_effort.as_deref(),
        Some("medium")
    );
    assert_eq!(models[0].reasoning_efforts, ["low", "medium", "high"]);
    assert_eq!(models[1].default_reasoning_effort.as_deref(), Some("high"));
    assert_eq!(models[1].reasoning_efforts, ["low", "high"]);
    facts.requests.lock().unwrap().clear();
    let configuration = RuntimeConfiguration::new()
        .with_model("openrouter/z-ai/glm-5.3")
        .unwrap()
        .with_reasoning_effort("high")
        .unwrap();
    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
        .unwrap()
        .with_configuration(configuration);
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    let selections = facts
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "session/set_config_option")
        .map(|request| request["params"]["configId"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(selections, ["model", "thought-next"]);
    session.close(CancellationToken::new()).await.unwrap();
}

#[tokio::test]
async fn acp_config_selection_requires_complete_applied_acknowledgement() {
    for mode in [FixtureMode::ConfigMissing, FixtureMode::ConfigNotApplied] {
        let workspace = std::env::current_dir().unwrap();
        let facts = Arc::new(FixtureFacts::default());
        let runtime = runtime(mode, Arc::clone(&facts), &workspace);
        let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
            .unwrap()
            .with_model("openrouter/z-ai/glm-5.3")
            .unwrap();
        let error = runtime
            .start(request, CancellationToken::new())
            .await
            .err()
            .expect("missing or unapplied configuration must fail");
        assert_eq!(error.code(), RuntimeErrorCode::Protocol);
        assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn acp_live_configuration_terminates_on_an_unapplied_acknowledgement() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(
        FixtureMode::ConfigNotApplied,
        Arc::clone(&facts),
        &workspace,
    );
    let session = runtime
        .start(
            RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let error = session
        .configure(
            RuntimeConfiguration::new()
                .with_model("openrouter/z-ai/glm-5.3")
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), RuntimeErrorCode::Protocol);
    assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
    session.close(CancellationToken::new()).await.unwrap();
}

#[tokio::test]
async fn acp_resume_rejects_unsupported_configuration_before_spawning() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::Permission, Arc::clone(&facts), &workspace);
    let configuration = RuntimeConfiguration::new()
        .with_system_prompt("unsupported ACP prompt")
        .unwrap();
    let request = RuntimeResume::new(
        heycode_core::SessionId::generate(),
        &workspace,
        RuntimeSessionId::new("fixture-session").unwrap(),
    )
    .unwrap()
    .with_configuration(configuration);
    let error = runtime
        .resume(request, CancellationToken::new())
        .await
        .err()
        .expect("unsupported configuration must fail before process launch");
    assert_eq!(error.code(), RuntimeErrorCode::Unsupported);
    assert!(facts.specs.lock().unwrap().is_empty());
}

#[async_trait]
impl AcpProcess for FixtureProcess {
    async fn write(
        &self,
        frame: &[u8],
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() || self.closed.load(Ordering::SeqCst) {
            return Err(RuntimeError::cancelled());
        }
        let value: Value = serde_json::from_slice(frame).map_err(|_| RuntimeError::protocol())?;
        self.facts.requests.lock().unwrap().push(value.clone());
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let id = value.get("id").cloned();
            match method {
                "initialize" => self.initialize(id.unwrap()),
                "authenticate" => {
                    assert_eq!(
                        value["params"],
                        serde_json::json!({"methodId":"cached_token","_meta":{"headless":true}})
                    );
                    if matches!(self.mode, FixtureMode::AuthRejected) {
                        self.enqueue(serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"private-auth-error"}}));
                    } else {
                        self.enqueue(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}));
                    }
                }
                "session/new" | "session/load" => self.session_response(id.unwrap()),
                "session/set_model" => {
                    assert!(matches!(self.mode, FixtureMode::LegacyModels));
                    let model = value["params"]["modelId"].as_str().unwrap().to_owned();
                    self.facts.selected_models.lock().unwrap().push(model);
                    self.enqueue(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}));
                }
                "session/set_config_option" => {
                    let selected = value["params"]["value"].as_str().unwrap().to_owned();
                    self.facts
                        .selected_models
                        .lock()
                        .unwrap()
                        .push(selected.clone());
                    match value["params"]["configId"].as_str().unwrap() {
                        "model" if !matches!(self.mode, FixtureMode::ConfigNotApplied) => {
                            let mut state = self.state.lock().unwrap();
                            state.model = selected;
                            if matches!(self.mode, FixtureMode::DependentEffort) {
                                state.effort = if state.model == "openrouter/z-ai/glm-5.3" {
                                    "high".to_owned()
                                } else {
                                    "medium".to_owned()
                                };
                            }
                        }
                        "thought" | "thought-next"
                            if !matches!(self.mode, FixtureMode::ConfigNotApplied) =>
                        {
                            self.state.lock().unwrap().effort = selected
                        }
                        "model" | "thought" | "thought-next" => {}
                        other => panic!("unexpected ACP config id {other}"),
                    }
                    if matches!(self.mode, FixtureMode::ConfigMissing) {
                        self.enqueue(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}));
                    } else {
                        self.config_options_response(id.unwrap());
                    }
                }
                "session/prompt" => self.begin_prompt(id.unwrap()),
                "session/cancel" => {
                    self.facts
                        .cancel_notifications
                        .fetch_add(1, Ordering::SeqCst);
                    let prompt_id = self.state.lock().unwrap().prompt_id.take();
                    if let Some(prompt_id) = prompt_id {
                        self.enqueue(serde_json::json!({
                            "jsonrpc":"2.0","id":prompt_id,
                            "result":{"stopReason":"cancelled"}
                        }));
                    }
                }
                "session/close" => self.enqueue(serde_json::json!({
                    "jsonrpc":"2.0","id":id,"result":{}
                })),
                other => panic!("unexpected ACP method {other}"),
            }
        } else if value.get("id") == Some(&serde_json::json!("permission-1")) {
            self.finish_permission_prompt(value);
        }
        Ok(())
    }

    async fn read(&self, cancellation: CancellationToken) -> Result<Option<Vec<u8>>, RuntimeError> {
        let mut receiver = self.receiver.lock().await;
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(RuntimeError::cancelled()),
            value = receiver.recv() => Ok(value.flatten()),
        }
    }

    async fn close(&self, _cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.facts.close_calls.fetch_add(1, Ordering::SeqCst);
            let _sent = self.sender.send(None);
        }
        Ok(())
    }
}

fn runtime(
    mode: FixtureMode,
    facts: Arc<FixtureFacts>,
    workspace: &Path,
) -> heycode_runtime::AcpRuntime {
    let executable = workspace.join("opencode-fixture");
    opencode_acp_runtime(
        Arc::new(FixtureFactory { facts, mode }),
        &executable,
        workspace,
        vec![(OsString::from("SAFE_NAME"), OsString::from("safe-value"))],
    )
    .unwrap()
}

#[tokio::test]
async fn opencode_profile_bridges_catalog_model_events_and_permission_exactly_once() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::Permission, Arc::clone(&facts), &workspace);
    let descriptor = runtime.descriptor();
    assert_eq!(descriptor.id().as_str(), "opencode");
    assert_eq!(
        descriptor.capabilities().models,
        CapabilitySupport::Supported
    );
    assert_eq!(
        descriptor.capabilities().resume,
        CapabilitySupport::Supported
    );
    assert_eq!(
        descriptor.capabilities().permissions,
        CapabilitySupport::Supported
    );
    assert_eq!(
        descriptor.capabilities().fork,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        descriptor.capabilities().steer,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        descriptor.capabilities().follow_up,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        descriptor.capabilities().questions,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        descriptor.capabilities().compaction,
        CapabilitySupport::Unsupported
    );

    let catalog = runtime.models(CancellationToken::new()).await.unwrap();
    assert_eq!(catalog.provider.id, "opencode");
    assert_eq!(catalog.models.len(), 2);
    assert!(
        catalog
            .models
            .iter()
            .any(|model| model.id == "opencode-go/glm-5.3-flash")
    );

    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
        .unwrap()
        .with_model("opencode-go/glm-5.3-flash")
        .unwrap();
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    let mut events = session.subscribe();
    let send_session = Arc::clone(&session);
    let send = tokio::spawn(async move {
        send_session
            .send(
                RuntimeInput::new("run the fixture tool").unwrap(),
                CancellationToken::new(),
            )
            .await
    });

    let mut observed = Vec::new();
    let permission_id = loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
            .await
            .expect("permission event timed out")
            .unwrap()
            .unwrap();
        observed.push(event.clone());
        if let RuntimeEventKind::PermissionRequested { request_id, .. } = event.kind() {
            break request_id.clone();
        }
    };

    let wrong = RuntimePermissionResponse::new(
        RuntimeRequestId::new("s:wrong").unwrap(),
        RuntimePermissionDecision::AllowOnce,
    );
    assert_eq!(
        session
            .respond_permission(wrong, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Conflict
    );
    assert!(facts.permission_answers.lock().unwrap().is_empty());

    let answer =
        RuntimePermissionResponse::new(permission_id.clone(), RuntimePermissionDecision::AllowOnce);
    session
        .respond_permission(answer.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        session
            .respond_permission(answer, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Conflict
    );
    let turn = send.await.unwrap().unwrap();
    assert_eq!(turn.as_str(), "1");

    while !observed
        .iter()
        .any(|event| matches!(event.kind(), RuntimeEventKind::TurnFinished { .. }))
    {
        observed.push(
            tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
                .await
                .expect("turn settlement timed out")
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        observed
            .iter()
            .map(heycode_runtime::RuntimeEvent::sequence)
            .collect::<Vec<_>>(),
        (0..observed.len() as u64).collect::<Vec<_>>()
    );
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
        |event| matches!(event.kind(), RuntimeEventKind::FinalMessage { text } if text == "done")
    ));
    assert_eq!(facts.permission_answers.lock().unwrap().len(), 1);
    assert_eq!(
        facts.selected_models.lock().unwrap().as_slice(),
        ["opencode-go/glm-5.3-flash"]
    );

    let specs = facts.specs.lock().unwrap().clone();
    assert_eq!(
        specs.len(),
        2,
        "catalog probe and primary session each own a process"
    );
    assert!(
        specs
            .iter()
            .all(|spec| spec.program == workspace.join("opencode-fixture"))
    );
    assert!(specs.iter().all(|spec| spec.cwd == workspace));
    assert!(
        specs
            .iter()
            .all(|spec| spec.args == [OsString::from("acp")])
    );
    assert!(
        specs
            .iter()
            .all(|spec| spec.environment_names == [OsString::from("SAFE_NAME")])
    );

    session.close(CancellationToken::new()).await.unwrap();
    session.close(CancellationToken::new()).await.unwrap();
    assert_eq!(
        facts.close_calls.load(Ordering::SeqCst),
        2,
        "one probe and one session process"
    );
}

#[tokio::test]
async fn acp_session_cancel_settles_pending_permission_and_process_close_once() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::Permission, Arc::clone(&facts), &workspace);
    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    let mut events = session.subscribe();
    let send_session = Arc::clone(&session);
    let send = tokio::spawn(async move {
        send_session
            .send(
                RuntimeInput::new("cancel me").unwrap(),
                CancellationToken::new(),
            )
            .await
    });
    let permission_id = loop {
        let event = events.next().await.unwrap().unwrap();
        if let RuntimeEventKind::PermissionRequested { request_id, .. } = event.kind() {
            break request_id.clone();
        }
    };
    session.cancel(CancellationToken::new()).await.unwrap();
    assert_eq!(
        send.await.unwrap().unwrap_err().code(),
        RuntimeErrorCode::Cancelled
    );
    assert_eq!(facts.cancel_notifications.load(Ordering::SeqCst), 1);
    {
        let answers = facts.permission_answers.lock().unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0]["result"]["outcome"]["outcome"], "cancelled");
    }
    let late = RuntimePermissionResponse::new(permission_id, RuntimePermissionDecision::AllowOnce);
    assert_eq!(
        session
            .respond_permission(late, CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Conflict
    );
    session.close(CancellationToken::new()).await.unwrap();
    session.close(CancellationToken::new()).await.unwrap();
    assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wrong_response_id_fails_protocol_and_quiescently_closes_process() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(
        FixtureMode::WrongInitializeId,
        Arc::clone(&facts),
        &workspace,
    );
    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
    let error = runtime
        .start(request, CancellationToken::new())
        .await
        .err()
        .expect("wrong response id must fail");
    assert_eq!(error.code(), RuntimeErrorCode::Protocol);
    assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn opencode_resume_uses_the_negotiated_load_session_and_exact_identity() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::Permission, Arc::clone(&facts), &workspace);
    let request = RuntimeResume::new(
        heycode_core::SessionId::generate(),
        &workspace,
        RuntimeSessionId::new("fixture-session").unwrap(),
    )
    .unwrap();
    let session = runtime
        .resume(request, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(session.id().as_str(), "fixture-session");
    let ready = session.subscribe().next().await.unwrap().unwrap();
    assert!(matches!(ready.kind(), RuntimeEventKind::SessionReady));
    session.close(CancellationToken::new()).await.unwrap();
    assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn acp_process_lifecycle_outlives_start_caller_and_runtime_shutdown_cancels_it() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::Permission, Arc::clone(&facts), &workspace);
    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
    let start_caller = CancellationToken::new();
    let session = runtime.start(request, start_caller.clone()).await.unwrap();
    let lifecycle = facts.lifecycles.lock().unwrap()[0].clone();

    start_caller.cancel();
    assert!(
        !lifecycle.is_cancelled(),
        "a completed start caller must not own the live process"
    );

    runtime.shutdown();
    assert!(
        lifecycle.is_cancelled(),
        "the runtime plugin lifecycle must own every ACP process"
    );
    session.close(CancellationToken::new()).await.unwrap();
    assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cached_authentication_requires_advertisement_and_success_and_closes_every_probe() {
    for (mode, authenticated) in [
        (FixtureMode::AuthReady, true),
        (FixtureMode::AuthAbsent, false),
        (FixtureMode::AuthRejected, false),
    ] {
        let workspace = std::env::current_dir().unwrap();
        let facts = Arc::new(FixtureFacts::default());
        let config = heycode_runtime::AcpRuntimeConfig::new(
            heycode_runtime::opencode_acp_descriptor().unwrap(),
            workspace.join("fixture"),
            vec!["acp".into()],
            vec![],
            &workspace,
            heycode_runtime::AccountState::without_label(heycode_runtime::AccountStatus::Unknown),
        )
        .unwrap()
        .with_cached_authentication("cached_token")
        .unwrap();
        let runtime = heycode_runtime::AcpRuntime::new(
            config,
            Arc::new(FixtureFactory {
                facts: facts.clone(),
                mode,
            }),
        );
        let account = runtime.account(CancellationToken::new()).await;
        if matches!(mode, FixtureMode::AuthRejected) {
            let error = account.unwrap_err();
            assert_eq!(error.code(), RuntimeErrorCode::Unavailable);
            assert!(!error.to_string().contains("private-auth-error"));
        } else {
            assert_eq!(
                account.unwrap().status() == heycode_runtime::AccountStatus::Connected,
                authenticated
            );
        }
        let requests = facts.requests.lock().unwrap();
        assert!(!requests.iter().any(|row| row["method"] == "session/new"));
        let attempts = requests
            .iter()
            .filter(|row| row["method"] == "authenticate")
            .count();
        assert_eq!(
            attempts,
            usize::from(!matches!(mode, FixtureMode::AuthAbsent))
        );
        assert_eq!(facts.close_calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn legacy_model_catalog_uses_its_exact_selection_method() {
    let workspace = std::env::current_dir().unwrap();
    let facts = Arc::new(FixtureFacts::default());
    let runtime = runtime(FixtureMode::LegacyModels, facts.clone(), &workspace);
    let catalog = runtime.models(CancellationToken::new()).await.unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["grok-4.5", "grok-4.6"]
    );
    let request = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
        .unwrap()
        .with_model("grok-4.5")
        .unwrap();
    let session = runtime
        .start(request, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*facts.selected_models.lock().unwrap(), ["grok-4.5"]);
    session.close(CancellationToken::new()).await.unwrap();
}

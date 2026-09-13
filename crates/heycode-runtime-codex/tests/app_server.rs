//! R03 Codex app-server exact process, handshake, correlation and teardown contracts.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used
)]

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::StreamExt as _;
    use heycode_core::ProviderProtocol;
    use heycode_exec::{
        InteractiveProcess, ManagedProcessHandle, ProcessError, ProcessErrorCode, ProcessOutput,
        ProcessSpec, RawInteractiveProcess, SandboxMode, SandboxService, SubprocessBackend,
        SubprocessContainment, SubprocessService,
    };
    use heycode_llm::{CapabilitySupport, ModelLifecycleStatus};
    use heycode_runtime::{AccountStatus, AgentRuntime, AgentRuntimeKind, AgentRuntimeRegistry};
    use heycode_runtime::{
        RuntimeCompactOutcome, RuntimeEventKind, RuntimeFinishReason, RuntimeFork, RuntimeInput,
        RuntimePermissionDecision, RuntimePermissionResponse, RuntimeQuestionResponse,
        RuntimeResume, RuntimeSessionId, RuntimeStart, RuntimeToolCall, RuntimeToolExecutor,
        RuntimeToolOutput,
    };
    use heycode_runtime_codex::{
        CodexAppServerConfig, CodexAppServerErrorCode, CodexClientInfo, CodexInboundEvent,
        CodexRuntime, SUPPORTED_CODEX_CLI_VERSION, codex_environment_snapshot,
        codex_runtime_plugin,
    };
    use serde_json::json;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct EchoToolExecutor {
        calls: Mutex<Vec<RuntimeToolCall>>,
    }

    #[derive(Default)]
    struct BlockingToolExecutor {
        calls: Mutex<Vec<RuntimeToolCall>>,
        entered: Notify,
    }

    #[async_trait]
    impl RuntimeToolExecutor for EchoToolExecutor {
        async fn execute(
            &self,
            call: RuntimeToolCall,
            _cancellation: CancellationToken,
        ) -> Result<RuntimeToolOutput, heycode_runtime::RuntimeError> {
            self.calls.lock().unwrap().push(call);
            Ok(RuntimeToolOutput {
                content: "tool-result".to_owned(),
                is_error: false,
            })
        }
    }

    #[async_trait]
    impl RuntimeToolExecutor for BlockingToolExecutor {
        async fn execute(
            &self,
            call: RuntimeToolCall,
            cancellation: CancellationToken,
        ) -> Result<RuntimeToolOutput, heycode_runtime::RuntimeError> {
            self.calls.lock().unwrap().push(call);
            self.entered.notify_one();
            cancellation.cancelled().await;
            Ok(RuntimeToolOutput {
                content: "cancelled".to_owned(),
                is_error: true,
            })
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        executable: PathBuf,
        workspace: PathBuf,
        log: PathBuf,
        marker: PathBuf,
        release: PathBuf,
    }

    impl Fixture {
        fn new(mode: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("codex-fixture");
            let workspace = root.path().join("workspace");
            let log = root.path().join("wire.log");
            let marker = root.path().join("descendant-survived");
            let release = root.path().join("version-release");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::write(&executable, fixture_script()).unwrap();
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable, permissions).unwrap();
            std::fs::write(root.path().join("mode"), mode).unwrap();
            Self {
                _root: root,
                executable,
                workspace,
                log,
                marker,
                release,
            }
        }

        fn config(&self, mode: &str) -> CodexAppServerConfig {
            CodexAppServerConfig::new(
                self.executable.as_os_str(),
                &self.workspace,
                [
                    (OsString::from("FAKE_CODEX_MODE"), OsString::from(mode)),
                    (
                        OsString::from("FAKE_CODEX_LOG"),
                        self.log.as_os_str().to_os_string(),
                    ),
                    (
                        OsString::from("FAKE_CODEX_MARKER"),
                        self.marker.as_os_str().to_os_string(),
                    ),
                    (
                        OsString::from("FAKE_CODEX_RELEASE"),
                        self.release.as_os_str().to_os_string(),
                    ),
                    (
                        OsString::from("FAKE_CODEX_WORKSPACE"),
                        self.workspace.as_os_str().to_os_string(),
                    ),
                    (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                ],
                CodexClientInfo::new("heycode", "heycode", "0.1.0").unwrap(),
            )
            .unwrap()
        }

        async fn wait_for_log(&self, needle: &str) {
            for _ in 0..500 {
                let text = std::fs::read_to_string(&self.log).unwrap_or_default();
                if text.contains(needle) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("fixture log did not contain expected safe marker");
        }

        /// Whether the contained process tree settled.
        ///
        /// Fixture descendants write the marker only after a delay or an
        /// explicit release, so absence is meaningful only after that window
        /// has passed. (A poll that returned as soon as the marker was absent
        /// proved nothing.)
        async fn process_tree_settled(&self) -> bool {
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            !self.marker.exists()
        }
    }

    struct SpawnStallBackend {
        local: SubprocessService,
        entered: Arc<Notify>,
        settled: Arc<AtomicBool>,
    }

    #[async_trait]
    impl SubprocessBackend for SpawnStallBackend {
        fn resolve_program(&self, program: &std::ffi::OsStr) -> Result<PathBuf, ProcessError> {
            self.local.resolve_program(program)
        }

        fn containment(&self) -> SubprocessContainment {
            self.local.containment()
        }

        async fn output(
            &self,
            spec: ProcessSpec,
            cancellation: CancellationToken,
        ) -> Result<ProcessOutput, ProcessError> {
            self.local.output(spec, cancellation).await
        }

        async fn spawn(
            &self,
            _spec: ProcessSpec,
            _cancellation: CancellationToken,
        ) -> Result<Box<dyn ManagedProcessHandle>, ProcessError> {
            Err(ProcessError::new(ProcessErrorCode::Unsupported))
        }

        async fn spawn_interactive(
            &self,
            _spec: ProcessSpec,
            cancellation: CancellationToken,
        ) -> Result<InteractiveProcess, ProcessError> {
            self.entered.notify_one();
            cancellation.cancelled().await;
            self.settled.store(true, Ordering::Release);
            Err(ProcessError::new(ProcessErrorCode::Cancelled))
        }

        async fn spawn_interactive_raw(
            &self,
            _spec: ProcessSpec,
            cancellation: CancellationToken,
        ) -> Result<RawInteractiveProcess, ProcessError> {
            self.entered.notify_one();
            cancellation.cancelled().await;
            self.settled.store(true, Ordering::Release);
            Err(ProcessError::new(ProcessErrorCode::Cancelled))
        }
    }

    #[tokio::test]
    async fn exact_version_handshake_bidirectional_ids_and_contained_group_close_pass() {
        let fixture = Fixture::new("normal");
        let subprocess = SubprocessService::local();
        let expected_containment = subprocess.containment();
        let runtime = CodexRuntime::new(subprocess, fixture.config("normal")).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        assert!(!format!("{client:?}").contains("app-server-stderr-canary"));
        assert_eq!(
            client.handshake().version().to_string(),
            SUPPORTED_CODEX_CLI_VERSION
        );
        assert_eq!(client.handshake().platform_family(), "unix");
        assert_eq!(client.handshake().platform_os(), "test-os");
        assert_eq!(client.containment(), &expected_containment);
        if client.containment().mechanism().as_str() == "process_group" {
            assert!(!client.containment().resists_session_escape());
        }

        let notification = client.next_event(CancellationToken::new()).await.unwrap();
        let CodexInboundEvent::Notification(notification) = notification else {
            panic!("first fixture event was not a notification");
        };
        assert_eq!(notification.method(), "fixture/ready");
        assert!(!format!("{notification:?}").contains("wire-secret-canary"));

        let request = client.next_event(CancellationToken::new()).await.unwrap();
        let CodexInboundEvent::Request(request) = request else {
            panic!("second fixture event was not a server request");
        };
        assert_eq!(request.method(), "fixture/approval");
        client
            .respond_success(
                request.id(),
                json!({"decision":"deny"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let duplicate = client
            .respond_success(request.id(), json!({}), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(duplicate.code(), CodexAppServerErrorCode::Conflict);

        let first_client = client.clone();
        let first = tokio::spawn(async move {
            first_client
                .request(
                    "probe/first",
                    json!({"private":"first-private-canary"}),
                    CancellationToken::new(),
                )
                .await
        });
        fixture.wait_for_log("probe/first").await;
        let second = client
            .request(
                "probe/second",
                json!({"private":"second-private-canary"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let first = first.await.unwrap().unwrap();
        assert_eq!(first.value()["name"], "first");
        assert_eq!(second.value()["name"], "second");

        let error = client
            .request("probe/error", json!({}), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Remote);
        assert!(!error.to_string().contains("remote-secret-canary"));
        assert!(!format!("{error:?}").contains("remote-secret-canary"));

        client
            .request("probe/arm", json!({}), CancellationToken::new())
            .await
            .unwrap();

        client.close(CancellationToken::new()).await.unwrap();
        client.close(CancellationToken::new()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(
            !fixture.marker.exists(),
            "descendant in the reported containment group survived close"
        );

        let log = std::fs::read_to_string(&fixture.log).unwrap();
        let lines = log.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "argv:--version");
        assert_eq!(lines[1], "argv:app-server --stdio --strict-config");
        let initialize = lines
            .iter()
            .find_map(|line| line.strip_prefix("stdin:").and_then(parse_initialize))
            .expect("initialize request in fixture log");
        assert_eq!(initialize["id"], 0);
        assert_eq!(initialize["method"], "initialize");
        assert!(initialize.get("jsonrpc").is_none());
        assert_eq!(initialize["params"]["clientInfo"]["name"], "heycode");
        assert!(log.contains("\"method\":\"initialized\""));
        assert!(log.contains("\"id\":\"server-1\",\"result\""));
    }

    #[tokio::test]
    async fn account_models_and_provider_capabilities_are_credential_blind_and_paginated() {
        let fixture = Fixture::new("discovery");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("discovery")).unwrap();
        assert_eq!(
            runtime.descriptor().capabilities().models,
            CapabilitySupport::Supported
        );

        let account = runtime.account(CancellationToken::new()).await.unwrap();
        assert_eq!(account.status(), AccountStatus::Connected);
        assert_eq!(account.label(), Some("ChatGPT pro"));

        let catalog = runtime.models(CancellationToken::new()).await.unwrap();
        assert_eq!(catalog.provider.id, "codex");
        assert_eq!(
            catalog.provider.protocols,
            [ProviderProtocol::DelegatedAgent]
        );
        assert_eq!(catalog.revision, 1);
        assert_eq!(catalog.models.len(), 2);
        let first = &catalog.models[0];
        assert_eq!(first.id, "codex-a");
        assert_eq!(first.aliases, ["codex-a-wire"]);
        assert_eq!(first.lifecycle.status, ModelLifecycleStatus::Deprecated);
        assert_eq!(first.lifecycle.replacement_ids, ["codex-b"]);
        assert_eq!(first.capabilities.reasoning, CapabilitySupport::Supported);
        assert_eq!(first.capabilities.image_input, CapabilitySupport::Supported);
        assert_eq!(first.capabilities.native_web, CapabilitySupport::Supported);
        assert_eq!(first.capabilities.tools, CapabilitySupport::Supported);
        let second = &catalog.models[1];
        assert_eq!(second.id, "codex-b");
        assert_eq!(
            second.capabilities.reasoning,
            CapabilitySupport::Unsupported
        );
        assert_eq!(
            second.capabilities.image_input,
            CapabilitySupport::Unsupported
        );

        let log = std::fs::read_to_string(&fixture.log).unwrap();
        assert!(log.contains("\"method\":\"account/read\""));
        assert!(log.contains("\"refreshToken\":false"));
        assert!(log.contains("\"method\":\"modelProvider/capabilities/read\""));
        assert_eq!(log.matches("\"method\":\"model/list\"").count(), 2);
        assert!(log.contains("\"includeHidden\":false"));
        assert!(log.contains("\"limit\":100"));
        assert!(!format!("{account:?}").contains("account-secret-canary"));
        assert!(!format!("{catalog:?}").contains("account-secret-canary"));
    }

    #[tokio::test]
    async fn account_read_distinguishes_signed_out_from_auth_not_required() {
        let signed_out = Fixture::new("discovery-signed-out");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            signed_out.config("discovery-signed-out"),
        )
        .unwrap();
        assert_eq!(
            runtime
                .account(CancellationToken::new())
                .await
                .unwrap()
                .status(),
            AccountStatus::Disconnected
        );

        let not_required = Fixture::new("discovery-not-required");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            not_required.config("discovery-not-required"),
        )
        .unwrap();
        assert_eq!(
            runtime
                .account(CancellationToken::new())
                .await
                .unwrap()
                .status(),
            AccountStatus::NotRequired
        );
    }

    #[tokio::test]
    async fn cancelled_account_read_settles_its_contained_process_before_returning() {
        let fixture = Fixture::new("discovery-stall");
        let runtime = Arc::new(
            CodexRuntime::new(
                SubprocessService::local(),
                fixture.config("discovery-stall"),
            )
            .unwrap(),
        );
        let cancellation = CancellationToken::new();
        let task_runtime = runtime.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move { task_runtime.account(task_cancellation).await });
        fixture.wait_for_log("descendant:ready").await;
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Cancelled);
        // The child cannot write the failure marker before cancellation. This
        // keeps host scheduling delays from being misreported as an orphan.
        std::fs::write(&fixture.release, b"release").unwrap();
        assert!(
            fixture.process_tree_settled().await,
            "account cancellation orphaned a process"
        );
    }

    #[tokio::test]
    async fn malformed_account_and_repeated_model_cursor_fail_body_free() {
        let malformed = Fixture::new("discovery-invalid-account");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            malformed.config("discovery-invalid-account"),
        )
        .unwrap();
        let error = runtime.account(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Protocol);
        assert!(!format!("{error:?}").contains("invalid-plan-secret-canary"));

        let repeated = Fixture::new("discovery-loop");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            repeated.config("discovery-loop"),
        )
        .unwrap();
        let error = runtime.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Protocol);
    }

    /// R02: a turn longer than the retained event window still settles, and a
    /// subscriber that attaches afterwards still receives a valid stream.
    #[tokio::test]
    async fn a_turn_longer_than_the_retained_window_settles_and_stays_subscribable() {
        let fixture = Fixture::new("primary-volume");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("primary-volume"))
                .unwrap();
        let start = RuntimeStart::new(
            heycode_core::SessionId::generate(),
            fixture.workspace.as_path(),
        )
        .unwrap();
        let session = runtime
            .start(start, CancellationToken::new())
            .await
            .unwrap();
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            events.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));
        session
            .send(
                RuntimeInput::new("stream a long answer").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut deltas = 0_usize;
        let reason = loop {
            let event = events.next().await.unwrap().unwrap();
            match event.kind() {
                RuntimeEventKind::CommentaryDelta { .. } => deltas += 1,
                RuntimeEventKind::TurnFinished { reason, .. } => break *reason,
                _ => {}
            }
        };
        assert_eq!(reason, heycode_runtime::RuntimeFinishReason::Stop);
        assert_eq!(deltas, 1_500);

        let mut late = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            late.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));
        let mut replayed = 1_usize;
        loop {
            let event = late.next().await.unwrap().unwrap();
            replayed += 1;
            if matches!(event.kind(), RuntimeEventKind::TurnFinished { .. }) {
                break;
            }
        }
        assert!(
            replayed < 1_500,
            "the retained window must stay bounded, replayed {replayed}"
        );
        session.close(CancellationToken::new()).await.unwrap();
    }

    /// A thread that completes without an agent message is still a stopped
    /// turn, so the adapter publishes the final message R02 requires instead
    /// of failing the whole session.
    #[tokio::test]
    async fn a_turn_that_completes_without_an_agent_message_still_settles() {
        let fixture = Fixture::new("primary-silent");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("primary-silent"))
                .unwrap();
        let start = RuntimeStart::new(
            heycode_core::SessionId::generate(),
            fixture.workspace.as_path(),
        )
        .unwrap();
        let session = runtime
            .start(start, CancellationToken::new())
            .await
            .unwrap();
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            events.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));
        session
            .send(
                RuntimeInput::new("say nothing").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut final_text = None;
        let reason = loop {
            let event = events.next().await.unwrap().unwrap();
            match event.kind() {
                RuntimeEventKind::FinalMessage { text } => final_text = Some(text.clone()),
                RuntimeEventKind::TurnFinished { reason, .. } => break *reason,
                _ => {}
            }
        };
        assert_eq!(reason, heycode_runtime::RuntimeFinishReason::Stop);
        assert_eq!(final_text.as_deref(), Some(""));
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn configured_session_bridges_dynamic_tools_before_reply() {
        let fixture = Fixture::new("primary-dynamic");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            fixture.config("primary-dynamic"),
        )
        .unwrap();
        let executor = Arc::new(EchoToolExecutor::default());
        let configuration = heycode_runtime::RuntimeConfiguration::new()
            .with_system_prompt("configured instructions")
            .unwrap()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo input".to_owned(),
                parameters: json!({"type":"object"}),
            }])
            .unwrap()
            .with_model("codex-a")
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap();
        let start = RuntimeStart::new(
            heycode_core::SessionId::generate(),
            fixture.workspace.as_path(),
        )
        .unwrap()
        .with_configuration(configuration)
        .with_tool_executor(executor.clone());
        let session = runtime
            .start(start, CancellationToken::new())
            .await
            .unwrap();
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        session
            .send(
                RuntimeInput::new("call echo").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut saw_call = false;
        let mut saw_result = false;
        loop {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::ToolCall { name, .. } if name == "echo" => saw_call = true,
                RuntimeEventKind::ToolResult { is_error, .. } => saw_result = !is_error,
                RuntimeEventKind::TurnFinished { .. } => break,
                _ => {}
            }
        }
        assert!(saw_call && saw_result);
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
        session.close(CancellationToken::new()).await.unwrap();
        let log = std::fs::read_to_string(&fixture.log).unwrap();
        assert!(log.contains("\"experimentalApi\":true"));
        assert!(log.contains("\"baseInstructions\":\"configured instructions\""));
        assert!(log.contains("\"dynamicTools\":[{\"description\":\"Echo input\""));
        assert!(log.contains("\"model_reasoning_effort\":\"high\""));
        assert!(log.contains("\"text\":\"tool-result\""));
    }

    #[tokio::test]
    async fn dynamic_tool_calls_are_limited_to_the_exact_configured_catalog() {
        let fixture = Fixture::new("primary-dynamic-unknown");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            fixture.config("primary-dynamic-unknown"),
        )
        .unwrap();
        let executor = Arc::new(EchoToolExecutor::default());
        let configuration = heycode_runtime::RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo input".to_owned(),
                parameters: json!({"type":"object"}),
            }])
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(
                    heycode_core::SessionId::generate(),
                    fixture.workspace.as_path(),
                )
                .unwrap()
                .with_configuration(configuration)
                .with_tool_executor(executor.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            events.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));
        session
            .send(
                RuntimeInput::new("call unknown").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = loop {
            match events.next().await {
                Some(Err(error)) => break error,
                Some(Ok(_)) => {}
                None => panic!("unknown dynamic tool did not fail the session"),
            }
        };
        assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Protocol);
        assert!(executor.calls.lock().unwrap().is_empty());
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_turn_retracts_its_blocked_dynamic_tool() {
        let fixture = Fixture::new("primary-dynamic-block");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            fixture.config("primary-dynamic-block"),
        )
        .unwrap();
        let executor = Arc::new(BlockingToolExecutor::default());
        let configuration = heycode_runtime::RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo input".to_owned(),
                parameters: json!({"type":"object"}),
            }])
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(
                    heycode_core::SessionId::generate(),
                    fixture.workspace.as_path(),
                )
                .unwrap()
                .with_configuration(configuration)
                .with_tool_executor(executor.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        session
            .send(
                RuntimeInput::new("call and block").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), executor.entered.notified())
            .await
            .expect("dynamic tool never started");
        tokio::time::timeout(
            Duration::from_secs(1),
            session.cancel(CancellationToken::new()),
        )
        .await
        .expect("turn cancellation did not retract the tool")
        .unwrap();
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn explicit_empty_tools_are_forwarded_and_rejected_on_resume_or_fork() {
        let fixture = Fixture::new("primary-silent");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("primary-silent"))
                .unwrap();
        let empty_tools = heycode_runtime::RuntimeConfiguration::new()
            .with_tools(Vec::new())
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(
                    heycode_core::SessionId::generate(),
                    fixture.workspace.as_path(),
                )
                .unwrap()
                .with_configuration(empty_tools.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        session.close(CancellationToken::new()).await.unwrap();
        let log = std::fs::read_to_string(&fixture.log).unwrap();
        assert!(log.contains("\"experimentalApi\":true"));
        assert!(log.contains("\"dynamicTools\":[]"));

        let resume = RuntimeResume::new(
            heycode_core::SessionId::generate(),
            fixture.workspace.as_path(),
            RuntimeSessionId::new("thread-existing").unwrap(),
        )
        .unwrap()
        .with_configuration(empty_tools.clone());
        assert_eq!(
            runtime
                .resume(resume, CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code(),
            heycode_runtime::RuntimeErrorCode::Unsupported
        );
        let fork = RuntimeFork::new(
            heycode_core::SessionId::generate(),
            fixture.workspace.as_path(),
            RuntimeSessionId::new("thread-existing").unwrap(),
        )
        .unwrap()
        .with_configuration(empty_tools);
        assert_eq!(
            runtime
                .fork(fork, CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code(),
            heycode_runtime::RuntimeErrorCode::Unsupported
        );
    }

    #[tokio::test]
    async fn live_configuration_sends_only_model_and_effort() {
        let fixture = Fixture::new("primary-silent");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("primary-silent"))
                .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(
                    heycode_core::SessionId::generate(),
                    fixture.workspace.as_path(),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let effective = session
            .configure(
                heycode_runtime::RuntimeConfiguration::new()
                    .with_model("codex-b")
                    .unwrap()
                    .with_reasoning_effort("low")
                    .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(effective.model(), Some("codex-b"));
        assert_eq!(effective.reasoning_effort(), Some("low"));
        let unsupported = heycode_runtime::RuntimeConfiguration::new()
            .with_system_prompt("launch only")
            .unwrap()
            .with_tools(Vec::new())
            .unwrap();
        let error = session
            .configure(unsupported, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Unsupported);
        assert_eq!(
            error.message(),
            "runtime configuration fields are unsupported: system_prompt, tools"
        );
        session.close(CancellationToken::new()).await.unwrap();
        let log = std::fs::read_to_string(&fixture.log).unwrap();
        assert!(log.contains("\"method\":\"thread/settings/update\""));
        assert!(log.contains("\"model\":\"codex-b\""));
        assert!(log.contains("\"effort\":\"low\""));
    }

    #[tokio::test]
    async fn primary_session_streams_steers_answers_permission_compacts_and_closes() {
        let fixture = Fixture::new("primary");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("primary")).unwrap();
        let capabilities = runtime.descriptor().capabilities();
        assert_eq!(capabilities.resume, CapabilitySupport::Supported);
        assert_eq!(capabilities.fork, CapabilitySupport::Supported);
        assert_eq!(capabilities.steer, CapabilitySupport::Supported);
        assert_eq!(capabilities.permissions, CapabilitySupport::Supported);
        assert_eq!(capabilities.questions, CapabilitySupport::Supported);
        assert_eq!(capabilities.compaction, CapabilitySupport::Supported);

        let start = RuntimeStart::new(
            heycode_core::SessionId::generate(),
            fixture.workspace.as_path(),
        )
        .unwrap()
        .with_model("codex-a")
        .unwrap();
        let session = runtime
            .start(start, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(session.id().as_str(), "thread-1");
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            events.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));

        let turn = session
            .send(
                RuntimeInput::new("do the work").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(turn.as_str(), "turn-1");
        let mut permission = None;
        let mut saw_commentary = false;
        let mut saw_reasoning = false;
        while permission.is_none() {
            let event = events.next().await.unwrap().unwrap();
            match event.kind() {
                RuntimeEventKind::CommentaryDelta { text } if text == "working" => {
                    saw_commentary = true;
                }
                RuntimeEventKind::ReasoningDelta { text } if text == "considering" => {
                    saw_reasoning = true;
                }
                RuntimeEventKind::PermissionRequested { request_id, .. } => {
                    permission = Some(request_id.clone());
                }
                _ => {}
            }
        }
        assert!(saw_commentary && saw_reasoning);
        session
            .steer(
                RuntimeInput::new("also verify tests").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        session
            .respond_permission(
                RuntimePermissionResponse::new(
                    permission.unwrap(),
                    RuntimePermissionDecision::AllowOnce,
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let question = loop {
            let event = events.next().await.unwrap().unwrap();
            if let RuntimeEventKind::QuestionRequested {
                request_id,
                prompt,
                choices,
                header,
                choice_descriptions,
                mode,
                progress,
                ..
            } = event.kind()
            {
                assert_eq!(header.as_deref(), Some("Confirm"));
                assert_eq!(
                    choice_descriptions,
                    &[Some("Run it".into()), Some("Skip it".into())]
                );
                assert_eq!(*mode, heycode_core::QuestionMode::SingleChoice);
                assert_eq!(*progress, (1, 3));
                assert_eq!(prompt, "Run the full suite?");
                assert_eq!(choices, &["Yes", "No"]);
                break request_id.clone();
            }
        };
        session
            .respond_question(
                RuntimeQuestionResponse::new(question.clone(), "Yes").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let second = loop {
            let event = events.next().await.unwrap().unwrap();
            if let RuntimeEventKind::QuestionRequested {
                request_id,
                mode,
                progress,
                choices,
                ..
            } = event.kind()
            {
                assert_ne!(request_id, &question);
                assert_eq!(*mode, heycode_core::QuestionMode::MultipleChoice);
                assert_eq!(*progress, (2, 3));
                assert_eq!(choices, &["Unit", "Integration"]);
                break request_id.clone();
            }
        };
        assert!(
            session
                .respond_question(
                    RuntimeQuestionResponse::new(question, "No").unwrap(),
                    CancellationToken::new()
                )
                .await
                .is_err(),
            "stale first-question reply must not answer the second question"
        );
        session
            .respond_question(
                RuntimeQuestionResponse::selected(
                    second,
                    vec!["Unit".into(), "Integration".into()],
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let third = loop {
            let event = events.next().await.unwrap().unwrap();
            if let RuntimeEventKind::QuestionRequested {
                request_id,
                mode,
                progress,
                choices,
                ..
            } = event.kind()
            {
                assert_eq!(*mode, heycode_core::QuestionMode::FreeText);
                assert_eq!(*progress, (3, 3));
                assert!(choices.is_empty());
                break request_id.clone();
            }
        };
        session
            .respond_question(
                RuntimeQuestionResponse::new(third, "Keep exact custom context").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut saw_final = false;
        let mut saw_usage = false;
        loop {
            let event = events.next().await.unwrap().unwrap();
            match event.kind() {
                RuntimeEventKind::FinalMessage { text } if text == "done" => saw_final = true,
                RuntimeEventKind::Usage { usage, .. }
                    if usage.prompt_tokens == 11 && usage.completion_tokens == 7 =>
                {
                    saw_usage = true;
                }
                RuntimeEventKind::TurnFinished { turn, reason } => {
                    assert_eq!(turn.as_str(), "turn-1");
                    assert_eq!(*reason, RuntimeFinishReason::Stop);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_final && saw_usage);
        assert_eq!(
            session.compact(CancellationToken::new()).await.unwrap(),
            RuntimeCompactOutcome::Applied
        );
        session.close(CancellationToken::new()).await.unwrap();

        let log = std::fs::read_to_string(&fixture.log).unwrap();
        for method in [
            "thread/start",
            "turn/start",
            "turn/steer",
            "thread/compact/start",
        ] {
            assert!(log.contains(&format!("\"method\":\"{method}\"")));
        }
        assert!(log.contains("\"decision\":\"accept\""));
        assert!(log.contains("\"question-1\""));
        assert!(log.contains("\"answers\":[\"Yes\"]"));
        assert!(log.contains("\"answers\":[\"Unit\",\"Integration\"]"));
        assert!(log.contains("Keep exact custom context"));
    }

    #[tokio::test]
    async fn primary_resume_fork_and_interrupt_are_owned_and_correlated() {
        let resume_fixture = Fixture::new("primary-resume");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            resume_fixture.config("primary-resume"),
        )
        .unwrap();
        let resume = RuntimeResume::new(
            heycode_core::SessionId::generate(),
            resume_fixture.workspace.as_path(),
            RuntimeSessionId::new("thread-existing").unwrap(),
        )
        .unwrap();
        let resumed = runtime
            .resume(resume, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resumed.id().as_str(), "thread-existing");
        resumed.close(CancellationToken::new()).await.unwrap();

        let fork_fixture = Fixture::new("primary-fork");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            fork_fixture.config("primary-fork"),
        )
        .unwrap();
        let fork = RuntimeFork::new(
            heycode_core::SessionId::generate(),
            fork_fixture.workspace.as_path(),
            RuntimeSessionId::new("thread-existing").unwrap(),
        )
        .unwrap();
        let forked = runtime.fork(fork, CancellationToken::new()).await.unwrap();
        assert_eq!(forked.id().as_str(), "thread-fork");
        let mut events = heycode_runtime::normalize_runtime_event_stream(forked.subscribe());
        events.next().await.unwrap().unwrap();
        forked
            .send(RuntimeInput::new("wait").unwrap(), CancellationToken::new())
            .await
            .unwrap();
        loop {
            if matches!(
                events.next().await.unwrap().unwrap().kind(),
                RuntimeEventKind::TurnStarted { .. }
            ) {
                break;
            }
        }
        forked.cancel(CancellationToken::new()).await.unwrap();
        loop {
            let event = events.next().await.unwrap().unwrap();
            if let RuntimeEventKind::TurnFinished { reason, .. } = event.kind() {
                assert_eq!(*reason, RuntimeFinishReason::Cancelled);
                break;
            }
        }
        forked.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_primary_send_closes_and_settles_the_owned_process() {
        let fixture = Fixture::new("primary-send-stall");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            fixture.config("primary-send-stall"),
        )
        .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(
                    heycode_core::SessionId::generate(),
                    fixture.workspace.as_path(),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let task_session = session.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            task_session
                .send(RuntimeInput::new("wait").unwrap(), task_cancellation)
                .await
        });
        fixture.wait_for_log("\"method\":\"turn/start\"").await;
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Cancelled);
        session.close(CancellationToken::new()).await.unwrap();
        assert!(
            fixture.process_tree_settled().await,
            "cancelled send orphaned a process"
        );
    }

    #[tokio::test]
    async fn unsupported_version_fails_before_app_server_spawn_without_echoing_output() {
        let fixture = Fixture::new("old-version");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("old-version")).unwrap();
        let error = runtime.connect(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::UnsupportedVersion);
        assert_eq!(error.to_string(), "Codex CLI version is unsupported");
        let log = std::fs::read_to_string(&fixture.log).unwrap();
        assert_eq!(log, "argv:--version\n");
        assert!(!format!("{error:?}").contains("0.145.0"));
        assert!(!format!("{error:?}").contains("private-version-canary"));
    }

    #[tokio::test]
    async fn successful_version_probe_tolerates_private_startup_diagnostics() {
        let fixture = Fixture::new("version-warning");
        let runtime = CodexRuntime::new(
            SubprocessService::local(),
            fixture
                .config("version-warning")
                .with_outer_sandbox_mode(SandboxMode::WorkspaceWrite),
        )
        .unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        assert_eq!(client.handshake().version().to_string(), "0.153.2");
        assert!(!format!("{client:?}").contains("private-version-warning"));
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn version_diagnostics_cannot_hide_bad_stdout_exit_or_truncation() {
        for mode in [
            "version-warning-old",
            "version-warning-failure",
            "version-warning-overflow",
        ] {
            let fixture = Fixture::new(mode);
            let runtime =
                CodexRuntime::new(SubprocessService::local(), fixture.config(mode)).unwrap();
            let error = runtime.connect(CancellationToken::new()).await.unwrap_err();
            let expected = if mode == "version-warning-overflow" {
                CodexAppServerErrorCode::Process
            } else {
                CodexAppServerErrorCode::UnsupportedVersion
            };
            assert_eq!(error.code(), expected, "{mode}");
            assert!(!format!("{error:?}").contains("private-version-warning"));
            assert_eq!(
                std::fs::read_to_string(&fixture.log).unwrap(),
                "argv:--version\n"
            );
        }
    }

    #[tokio::test]
    async fn restricted_startup_exit_reports_state_directory_guidance() {
        for mode in [
            SandboxMode::ReadOnly,
            SandboxMode::WorkspaceWrite,
            SandboxMode::Off,
        ] {
            let fixture = Fixture::new("startup-exit");
            let runtime = CodexRuntime::new(
                SubprocessService::local(),
                fixture.config("startup-exit").with_outer_sandbox_mode(mode),
            )
            .unwrap();
            let error = runtime.connect(CancellationToken::new()).await.unwrap_err();
            if mode == SandboxMode::Off {
                assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
            } else {
                assert_eq!(error.code(), CodexAppServerErrorCode::SandboxStartup);
                assert!(error.to_string().contains("official state directory"));
                assert!(error.to_string().contains("--sandbox off"));
            }
            assert!(!format!("{error:?}").contains("private-startup-canary"));
        }
    }

    #[tokio::test]
    async fn ambient_path_cannot_select_a_workspace_shebang_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let bin = workspace.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let executable = root.path().join("codex-script");
        std::fs::write(&executable, "#!/usr/bin/env node\n").unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let marker = root.path().join("workspace-node-ran");
        let node = bin.join("node");
        std::fs::write(
            &node,
            format!(
                "#!/bin/sh\nprintf hijacked > '{}'\nif [ \"$2\" = \"--version\" ]; then printf '%s\\n' 'codex-cli 0.153.2'; exit 0; fi\n",
                marker.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&node).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&node, permissions).unwrap();
        let config = CodexAppServerConfig::new(
            &executable,
            &workspace,
            [(OsString::from("PATH"), bin.as_os_str().to_os_string())],
            CodexClientInfo::new("heycode", "heycode", "0.1.0").unwrap(),
        )
        .unwrap();
        let runtime = CodexRuntime::new(SubprocessService::local(), config).unwrap();
        let error = runtime.connect(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::InvalidConfig);
        assert!(
            !marker.exists(),
            "workspace interpreter executed before refusal"
        );
    }

    #[tokio::test]
    async fn sanitized_path_supports_a_bound_env_node_launcher() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let trusted_bin = root.path().join("trusted-bin");
        let executable = root.path().join("codex-script");
        let log = root.path().join("wire.log");
        let marker = root.path().join("marker");
        let release = root.path().join("release");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&trusted_bin).unwrap();
        std::os::unix::fs::symlink("/bin/sh", trusted_bin.join("node")).unwrap();
        std::fs::write(
            &executable,
            fixture_script().replacen("#!/bin/sh", "#!/usr/bin/env node", 1),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let config = CodexAppServerConfig::new(
            &executable,
            &workspace,
            [
                (OsString::from("FAKE_CODEX_MODE"), OsString::from("normal")),
                (
                    OsString::from("FAKE_CODEX_LOG"),
                    log.as_os_str().to_os_string(),
                ),
                (
                    OsString::from("FAKE_CODEX_MARKER"),
                    marker.as_os_str().to_os_string(),
                ),
                (
                    OsString::from("FAKE_CODEX_RELEASE"),
                    release.as_os_str().to_os_string(),
                ),
                (
                    OsString::from("PATH"),
                    std::env::join_paths([root.path().join("missing-bin"), trusted_bin.clone()])
                        .unwrap(),
                ),
            ],
            CodexClientInfo::new("heycode", "heycode", "0.1.0").unwrap(),
        )
        .unwrap();
        let runtime = CodexRuntime::new(SubprocessService::local(), config).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        assert_eq!(client.handshake().version().to_string(), "0.153.2");
        client.close(CancellationToken::new()).await.unwrap();
        assert!(
            std::fs::read_to_string(log)
                .unwrap()
                .contains("argv:--version")
        );
    }

    #[tokio::test]
    async fn nested_env_shebang_cannot_reintroduce_ambient_interpreter_lookup() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let trusted_bin = root.path().join("trusted-bin");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&trusted_bin).unwrap();
        let executable = root.path().join("codex-script");
        std::fs::write(&executable, "#!/usr/bin/env node\n").unwrap();
        let node = trusted_bin.join("node");
        let marker = root.path().join("nested-interpreter-ran");
        std::fs::write(
            &node,
            format!(
                "#!/usr/bin/env sh\nprintf unsafe > '{}'\n",
                marker.display()
            ),
        )
        .unwrap();
        for path in [&executable, &node] {
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(path, permissions).unwrap();
        }
        let config = CodexAppServerConfig::new(
            &executable,
            &workspace,
            [(
                OsString::from("PATH"),
                trusted_bin.as_os_str().to_os_string(),
            )],
            CodexClientInfo::new("heycode", "heycode", "0.1.0").unwrap(),
        )
        .unwrap();
        let runtime = CodexRuntime::new(SubprocessService::local(), config).unwrap();
        let error = runtime.connect(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::InvalidConfig);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn replacement_between_version_and_app_server_fails_closed() {
        let fixture = Fixture::new("version-barrier");
        let runtime = Arc::new(
            CodexRuntime::new(
                SubprocessService::local(),
                fixture.config("version-barrier"),
            )
            .unwrap(),
        );
        let task = tokio::spawn(async move { runtime.connect(CancellationToken::new()).await });
        fixture.wait_for_log("argv:--version").await;

        let replacement = fixture._root.path().join("codex-replacement");
        std::fs::write(&replacement, format!("{}\n# replacement", fixture_script())).unwrap();
        let mut permissions = std::fs::metadata(&replacement).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&replacement, permissions).unwrap();
        std::fs::rename(&replacement, &fixture.executable).unwrap();
        std::fs::write(&fixture.release, "continue").unwrap();

        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::UnsupportedVersion);
        let log = std::fs::read_to_string(&fixture.log).unwrap();
        assert_eq!(
            log.lines().filter(|line| line.starts_with("argv:")).count(),
            1
        );
    }

    #[tokio::test]
    async fn caller_cancel_during_spawn_cancels_and_settles_spawn() {
        let fixture = Fixture::new("normal");
        let entered = Arc::new(Notify::new());
        let settled = Arc::new(AtomicBool::new(false));
        let subprocess = SubprocessService::new(Arc::new(SpawnStallBackend {
            local: SubprocessService::local(),
            entered: entered.clone(),
            settled: settled.clone(),
        }));
        let runtime = Arc::new(CodexRuntime::new(subprocess, fixture.config("normal")).unwrap());
        let cancellation = CancellationToken::new();
        let task_runtime = runtime.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move { task_runtime.connect(task_cancellation).await });
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Cancelled);
        assert!(settled.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn handshake_cancellation_settles_the_process_tree_before_returning() {
        let fixture = Fixture::new("stall");
        let runtime = Arc::new(
            CodexRuntime::new(SubprocessService::local(), fixture.config("stall")).unwrap(),
        );
        let cancellation = CancellationToken::new();
        let child_cancellation = cancellation.clone();
        let task = tokio::spawn(async move { runtime.connect(child_cancellation).await });
        fixture.wait_for_log("\"method\":\"initialize\"").await;
        cancellation.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Cancelled);
        assert!(
            fixture.process_tree_settled().await,
            "cancel returned before tree settlement"
        );
    }

    #[tokio::test]
    async fn cancellation_during_blocked_jsonl_write_reaps_tree_before_returning() {
        let fixture = Fixture::new("write-stall");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("write-stall")).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        fixture.wait_for_log("\"method\":\"initialized\"").await;
        let cancellation = CancellationToken::new();
        let task_client = client.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            task_client
                .request(
                    "probe/blocked-write",
                    json!({"payload":"x".repeat(512 * 1024)}),
                    task_cancellation,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Cancelled);
        assert!(
            fixture.process_tree_settled().await,
            "blocked write left descendant alive"
        );
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn malformed_provider_line_fails_with_a_body_free_protocol_error() {
        let fixture = Fixture::new("malformed");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("malformed")).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        let error = client
            .next_event(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
        assert_eq!(error.to_string(), "Codex app-server protocol failed");
        assert!(!format!("{error:?}").contains("protocol-secret-canary"));
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(
            !fixture.marker.exists(),
            "protocol error returned before tree settlement"
        );
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn long_lived_stdout_exceeding_one_mib_keeps_later_frames_visible() {
        let fixture = Fixture::new("many-frames");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("many-frames")).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        for expected in 0..200_u64 {
            let event = tokio::time::timeout(
                Duration::from_secs(3),
                client.next_event(CancellationToken::new()),
            )
            .await
            .expect("later raw frame was silently dropped")
            .unwrap();
            let CodexInboundEvent::Notification(notification) = event else {
                panic!("long-stream frame was not a notification");
            };
            assert_eq!(notification.method(), "fixture/frame");
            assert_eq!(notification.params()["index"], expected);
            assert_eq!(notification.params()["body"].as_str().unwrap().len(), 8_192);
        }
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_utf8_from_the_real_raw_process_path_is_terminal() {
        let fixture = Fixture::new("invalid-utf8");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("invalid-utf8")).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            client.next_event(CancellationToken::new()),
        )
        .await
        .expect("invalid UTF-8 did not settle the connection")
        .unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_request_discards_one_late_response_and_connection_remains_reusable() {
        let fixture = Fixture::new("normal");
        let runtime =
            CodexRuntime::new(SubprocessService::local(), fixture.config("normal")).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        let cancellation = CancellationToken::new();
        let task_client = client.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            task_client
                .request("probe/slow", json!({}), task_cancellation)
                .await
        });
        fixture.wait_for_log("probe/slow").await;
        cancellation.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Cancelled);

        let released = client
            .request("probe/release", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(released.value()["released"], true);
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_connections_keep_process_and_request_identity_isolated() {
        let first_fixture = Fixture::new("normal");
        let second_fixture = Fixture::new("normal");
        let first =
            CodexRuntime::new(SubprocessService::local(), first_fixture.config("normal")).unwrap();
        let second =
            CodexRuntime::new(SubprocessService::local(), second_fixture.config("normal")).unwrap();
        let (first, second) = tokio::join!(
            first.connect(CancellationToken::new()),
            second.connect(CancellationToken::new())
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.handshake().platform_os(), "test-os");
        assert_eq!(second.handshake().platform_os(), "test-os");
        let (first_close, second_close) = tokio::join!(
            first.close(CancellationToken::new()),
            second.close(CancellationToken::new())
        );
        first_close.unwrap();
        second_close.unwrap();
    }

    #[tokio::test]
    async fn plugin_registers_runtime_without_eager_account_or_model_discovery() {
        let fixture = Fixture::new("normal");
        let cwd = std::env::current_dir().unwrap();
        let sandbox = SandboxService::new(SandboxMode::Off, &cwd, None).unwrap();
        let plugins = vec![
            heycode_runtime::runtime_registry_plugin(),
            heycode_exec::sandbox_service_plugin(sandbox),
            heycode_exec::local_subprocess_plugin(),
            codex_runtime_plugin(fixture.config("normal")),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let registry = context
            .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
            .unwrap();
        let descriptor = registry
            .descriptors()
            .unwrap()
            .into_iter()
            .find(|descriptor| descriptor.id().as_str() == "codex")
            .unwrap();
        assert_eq!(descriptor.kind(), AgentRuntimeKind::Delegated);
        let capabilities = descriptor.capabilities();
        for support in [
            capabilities.models,
            capabilities.resume,
            capabilities.fork,
            capabilities.steer,
            capabilities.permissions,
            capabilities.questions,
            capabilities.compaction,
        ] {
            assert_eq!(support, CapabilitySupport::Supported);
        }
        assert_eq!(capabilities.follow_up, CapabilitySupport::Unsupported);
        assert!(
            context
                .plugin_inventory()
                .snapshot()
                .unwrap()
                .contributions
                .iter()
                .any(
                    |row| row.kind == heycode_core::ContributionKind::AgentRuntime
                        && row.name == "codex"
                        && row.plugin == "runtime-codex"
                )
        );
        let runtime = registry.get("codex").unwrap().unwrap();
        assert!(
            !fixture.log.exists(),
            "plugin registration invoked Codex discovery"
        );
        context.shutdown();
        assert!(registry.get("codex").unwrap().is_none());
        assert_eq!(
            runtime
                .account(CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            heycode_runtime::RuntimeErrorCode::Unavailable
        );
    }

    #[test]
    fn client_and_config_boundaries_reject_control_text_and_relative_workspaces() {
        assert!(CodexClientInfo::new("bad\nname", "title", "1").is_err());
        assert!(CodexClientInfo::new("good", " bad ", "1").is_err());
        assert!(
            CodexAppServerConfig::new(
                "codex",
                Path::new("relative"),
                std::iter::empty::<(OsString, OsString)>(),
                CodexClientInfo::new("heycode", "heycode", "1").unwrap(),
            )
            .is_err()
        );
        let cwd = std::env::current_dir().unwrap();
        let redacted = CodexAppServerConfig::new(
            "codex",
            &cwd,
            [(
                OsString::from("PRIVATE_VALUE"),
                OsString::from("config-secret-canary"),
            )],
            CodexClientInfo::new("heycode", "private-title-canary", "1").unwrap(),
        )
        .unwrap();
        let debug = format!("{redacted:?}");
        assert!(!debug.contains("config-secret-canary"));
        assert!(!debug.contains("private-title-canary"));
        assert!(
            CodexAppServerConfig::new(
                "codex",
                &cwd,
                [
                    (OsString::from("DUPLICATE"), OsString::from("one")),
                    (OsString::from("DUPLICATE"), OsString::from("two")),
                ],
                CodexClientInfo::new("heycode", "heycode", "1").unwrap(),
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn configured_local_codex_0153_handshake_is_gated_and_credential_blind() {
        if std::env::var("HEYCODE_CODEX_E2E").ok().as_deref() != Some("1") {
            return;
        }
        let service = SubprocessService::local();
        let executable = service.resolve_program("codex".as_ref()).unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let config = CodexAppServerConfig::new(
            executable.as_os_str(),
            cwd,
            [
                (
                    OsString::from("CODEX_HOME"),
                    codex_home.path().as_os_str().to_os_string(),
                ),
                (
                    OsString::from("PATH"),
                    std::env::var_os("PATH").unwrap_or_default(),
                ),
            ],
            CodexClientInfo::new("heycode_r03_test", "heycode R03 Test", "0.1.0").unwrap(),
        )
        .unwrap();
        let runtime = CodexRuntime::new(service, config).unwrap();
        let client = runtime.connect(CancellationToken::new()).await.unwrap();
        assert_eq!(client.handshake().version().to_string(), "0.153.2");
        client.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn configured_local_codex_0153_account_and_models_are_gated_and_credential_blind() {
        if std::env::var("HEYCODE_CODEX_ACCOUNT_E2E").ok().as_deref() != Some("1") {
            return;
        }
        let service = SubprocessService::local();
        let executable = service.resolve_program("codex".as_ref()).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let config = CodexAppServerConfig::new(
            executable.as_os_str(),
            cwd,
            codex_environment_snapshot(),
            CodexClientInfo::new("heycode_r04_test", "heycode R04 Test", "0.1.0").unwrap(),
        )
        .unwrap();
        let runtime = CodexRuntime::new(service, config).unwrap();
        let account = runtime.account(CancellationToken::new()).await.unwrap();
        assert_eq!(account.status(), AccountStatus::Connected);
        let models = runtime.models(CancellationToken::new()).await.unwrap();
        assert!(!models.models.is_empty());
        assert_eq!(models.provider.id, "codex");
        assert!(
            models
                .models
                .iter()
                .all(|model| !model.id.is_empty() && !model.display_name.is_empty())
        );
    }

    fn parse_initialize(line: &str) -> Option<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        (value.get("method").and_then(serde_json::Value::as_str) == Some("initialize"))
            .then_some(value)
    }

    /// Uses the official installed subscription only when explicitly opted in.
    /// Exercises real streaming and the dynamic tool round trip, not just the
    /// handshake/model catalog covered by the other installed-runtime canaries.
    #[tokio::test]
    async fn configured_local_codex_primary_turn_and_tool_are_explicitly_gated() {
        if std::env::var("HEYCODE_CODEX_SESSION_E2E").ok().as_deref() != Some("1") {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let config = CodexAppServerConfig::new(
            "codex",
            workspace.path(),
            codex_environment_snapshot(),
            CodexClientInfo::new("heycode_session_test", "heycode Session Test", "0.1.0").unwrap(),
        )
        .unwrap();
        let runtime = CodexRuntime::new(SubprocessService::local(), config).unwrap();
        let executor = Arc::new(EchoToolExecutor::default());
        let model =
            std::env::var("HEYCODE_CODEX_MODEL").unwrap_or_else(|_| "gpt-6-astra".to_owned());
        let configuration = heycode_runtime::RuntimeConfiguration::new()
            .with_model(model)
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Return the fixed public test result tool-result.".to_owned(),
                parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
            }])
            .unwrap();
        let cancellation = CancellationToken::new();
        let session = tokio::time::timeout(
            Duration::from_secs(45),
            runtime.start(
                RuntimeStart::new(heycode_core::SessionId::generate(), workspace.path())
                    .unwrap()
                    .with_ephemeral()
                    .with_configuration(configuration)
                    .with_tool_executor(executor.clone()),
                cancellation.clone(),
            ),
        )
        .await
        .expect("installed Codex session start timed out")
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
            for (prompt, expected) in [
                ("Reply exactly HEYCODE_CODEX_PRIMARY_OK. Do not use any tools.", "HEYCODE_CODEX_PRIMARY_OK"),
                ("Call the provided echo tool exactly once with {}. Do not call any other tools. Then reply with its result exactly.", "tool-result"),
            ] {
                session.send(RuntimeInput::new(prompt).unwrap(), cancellation.clone()).await?;
                let mut final_text = None;
                loop {
                    let event = events.next().await.expect("installed Codex event stream ended")?;
                    match event.kind() {
                        RuntimeEventKind::FinalMessage { text } => final_text = Some(text.clone()),
                        RuntimeEventKind::TurnFinished { reason, .. } => {
                            assert_eq!(*reason, RuntimeFinishReason::Stop);
                            assert_eq!(final_text.as_deref().map(str::trim), Some(expected));
                            break;
                        }
                        RuntimeEventKind::PermissionRequested { .. } | RuntimeEventKind::QuestionRequested { .. } => {
                            panic!("tool-free/in-memory canary unexpectedly required user input");
                        }
                        _ => {}
                    }
                }
            }
            assert_eq!(executor.calls.lock().unwrap().len(), 1);
            let interrupted = session.send(RuntimeInput::new(
                "Write a detailed 10000-word guide to sorting algorithms. Do not use tools."
            ).unwrap(), cancellation.clone()).await?;
            session.cancel(CancellationToken::new()).await?;
            loop {
                let event = events.next().await.expect("cancelled Codex event stream ended")?;
                if let RuntimeEventKind::TurnFinished { turn, reason } = event.kind() {
                    assert_eq!(turn, &interrupted);
                    assert_eq!(*reason, RuntimeFinishReason::Cancelled);
                    break;
                }
            }
            // A late subscriber must retain the settled terminal state.
            let mut replay = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
            loop {
                let event = replay.next().await.expect("Codex replay ended")?;
                if let RuntimeEventKind::TurnFinished { turn, reason } = event.kind()
                    && turn == &interrupted {
                    assert_eq!(*reason, RuntimeFinishReason::Cancelled);
                    break;
                }
            }
            Ok::<(), heycode_runtime::RuntimeError>(())
        }).await;
        cancellation.cancel();
        session.close(CancellationToken::new()).await.unwrap();
        result
            .expect("installed Codex session turn timed out")
            .unwrap();
    }

    fn fixture_script() -> &'static str {
        r#"#!/bin/sh
printf 'argv:%s\n' "$*" >> "$FAKE_CODEX_LOG"
if [ "$1" = "--version" ]; then
  case "$FAKE_CODEX_MODE" in
    version-warning*)
      printf '%s\n' 'WARNING: proceeding, even though we could not create PATH aliases: Operation not permitted (os error 1) private-version-warning' >&2
      ;;
  esac
  if [ "$FAKE_CODEX_MODE" = "version-warning-overflow" ]; then
    i=0
    while [ "$i" -lt 100 ]; do
      printf '%s\n' 'private-version-warning-output-overflow-private-version-warning' >&2
      i=$((i+1))
    done
  fi
  if [ "$FAKE_CODEX_MODE" = "version-barrier" ]; then
    while [ ! -f "$FAKE_CODEX_RELEASE" ]; do sleep 0.01; done
  fi
  if [ "$FAKE_CODEX_MODE" = "old-version" ] || [ "$FAKE_CODEX_MODE" = "version-warning-old" ]; then
    printf '%s\n' 'codex-cli 0.145.0 private-version-canary'
  else
    printf '%s\n' 'codex-cli 0.153.2'
  fi
  if [ "$FAKE_CODEX_MODE" = "version-warning-failure" ]; then exit 1; fi
  exit 0
fi
if [ "$1" != "app-server" ] || [ "$2" != "--stdio" ] || [ "$3" != "--strict-config" ]; then
  exit 64
fi
printf '%s\n' 'app-server-stderr-canary' >&2
if [ "$FAKE_CODEX_MODE" = "startup-exit" ]; then
  printf '%s\n' 'private-startup-canary' >&2
  exit 1
fi
while IFS= read -r line; do
  printf 'stdin:%s\n' "$line" >> "$FAKE_CODEX_LOG"
  case "$line" in
    *'"method":"initialize"'*)
      if [ "$FAKE_CODEX_MODE" = "stall" ]; then
        (sleep 1; printf survived > "$FAKE_CODEX_MARKER") &
      else
        printf '%s\n' '{"id":0,"result":{"codexHome":"/private/redacted-codex-home","platformFamily":"unix","platformOs":"test-os","userAgent":"Codex Desktop/0.153.2 (fixture)"}}'
      fi
      ;;
    *'"method":"initialized"'*)
      if [ "$FAKE_CODEX_MODE" = "malformed" ]; then
        (sleep 0.25; printf survived > "$FAKE_CODEX_MARKER") &
        printf '%s\n' 'not-json protocol-secret-canary'
      elif [ "$FAKE_CODEX_MODE" = "write-stall" ]; then
        (sleep 1; printf survived > "$FAKE_CODEX_MARKER") &
        sleep 10
      elif [ "$FAKE_CODEX_MODE" = "many-frames" ]; then
        body=$(printf '%08192d' 0)
        i=0
        while [ "$i" -lt 200 ]; do
          printf '{"method":"fixture/frame","params":{"index":%s,"body":"%s"}}\n' "$i" "$body"
          i=$((i + 1))
        done
      elif [ "$FAKE_CODEX_MODE" = "invalid-utf8" ]; then
        printf '{"method":"fixture/invalid","params":{"body":"\377"}}\n'
      elif [ "${FAKE_CODEX_MODE#discovery}" != "$FAKE_CODEX_MODE" ]; then
        :
      elif [ "${FAKE_CODEX_MODE#primary}" != "$FAKE_CODEX_MODE" ]; then
        :
      else
        printf '%s\n' '{"method":"fixture/ready","params":{"private":"wire-secret-canary"}}'
        printf '%s\n' '{"id":"server-1","method":"fixture/approval","params":{"private":"approval-secret-canary"}}'
      fi
      ;;
    *'"method":"account/read"'*)
      if [ "$FAKE_CODEX_MODE" = "discovery" ]; then
        printf '%s\n' '{"id":1,"result":{"account":{"type":"chatgpt","email":"account-secret-canary@example.com","planType":"pro"},"requiresOpenaiAuth":true}}'
      elif [ "$FAKE_CODEX_MODE" = "discovery-signed-out" ]; then
        printf '%s\n' '{"id":1,"result":{"account":null,"requiresOpenaiAuth":true}}'
      elif [ "$FAKE_CODEX_MODE" = "discovery-not-required" ]; then
        printf '%s\n' '{"id":1,"result":{"account":null,"requiresOpenaiAuth":false}}'
      elif [ "$FAKE_CODEX_MODE" = "discovery-stall" ]; then
        (printf '%s\n' 'descendant:ready' >> "$FAKE_CODEX_LOG"; while [ ! -f "$FAKE_CODEX_RELEASE" ]; do sleep 0.01; done; printf survived > "$FAKE_CODEX_MARKER") &
      elif [ "$FAKE_CODEX_MODE" = "discovery-invalid-account" ]; then
        printf '%s\n' '{"id":1,"result":{"account":{"type":"chatgpt","email":null,"planType":"invalid-plan-secret-canary"},"requiresOpenaiAuth":true}}'
      fi
      ;;
    *'"method":"modelProvider/capabilities/read"'*)
      printf '%s\n' '{"id":1,"result":{"webSearch":true,"imageGeneration":false,"namespaceTools":true}}'
      ;;
    *'"method":"model/list"'*)
      if [ "$FAKE_CODEX_MODE" = "discovery-loop" ]; then
        case "$line" in
          *'"cursor":"next"'*)
            printf '%s\n' '{"id":3,"result":{"data":[],"nextCursor":"next"}}'
            ;;
          *)
            printf '%s\n' '{"id":2,"result":{"data":[],"nextCursor":"next"}}'
            ;;
        esac
        continue
      fi
      case "$line" in
        *'"cursor":"next"'*)
          printf '%s\n' '{"id":3,"result":{"data":[{"id":"codex-b","model":"codex-b","displayName":"Codex B","description":"Stable model","hidden":false,"defaultReasoningEffort":"none","supportedReasoningEfforts":[],"inputModalities":["text"],"supportsPersonality":false,"isDefault":false}],"nextCursor":null}}'
          ;;
        *)
          printf '%s\n' '{"id":2,"result":{"data":[{"id":"codex-a","model":"codex-a-wire","displayName":"Codex A","description":"Reasoning image model","hidden":false,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"Fast"},{"reasoningEffort":"high","description":"Deep"}],"inputModalities":["text","image"],"supportsPersonality":true,"isDefault":true,"upgrade":"codex-b"}],"nextCursor":"next"}}'
          ;;
      esac
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":1,"result":{"thread":{"id":"thread-1"},"model":"codex-a","modelProvider":"openai","cwd":"%s","approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":"read-only","reasoningEffort":"high"}}\n' "$FAKE_CODEX_WORKSPACE"
      printf '%s\n' '{"method":"thread/started","params":{"thread":{"id":"thread-1"}}}'
      ;;
    *'"method":"thread/resume"'*)
      printf '{"id":1,"result":{"thread":{"id":"thread-existing"},"model":"codex-a","modelProvider":"openai","cwd":"%s","approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":"read-only"}}\n' "$FAKE_CODEX_WORKSPACE"
      ;;
    *'"method":"thread/fork"'*)
      printf '{"id":1,"result":{"thread":{"id":"thread-fork"},"model":"codex-a","modelProvider":"openai","cwd":"%s","approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":"read-only"}}\n' "$FAKE_CODEX_WORKSPACE"
      ;;
    *'"method":"turn/start"'*)
      if [ "$FAKE_CODEX_MODE" = "primary-send-stall" ]; then
        (sleep 1; printf survived > "$FAKE_CODEX_MARKER") &
        continue
      fi
      printf '%s\n' '{"id":2,"result":{"turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
      if [ "${FAKE_CODEX_MODE#primary-dynamic}" != "$FAKE_CODEX_MODE" ]; then
        tool=echo
        if [ "$FAKE_CODEX_MODE" = "primary-dynamic-unknown" ]; then tool=unknown; fi
        printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
        printf '{"id":"tool-1","method":"item/tool/call","params":{"threadId":"thread-1","turnId":"turn-1","callId":"call-1","namespace":null,"tool":"%s","arguments":{"text":"hello"}}}\n' "$tool"
      elif [ "$FAKE_CODEX_MODE" = "primary-volume" ]; then
        printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
        emitted=0
        while [ "$emitted" -lt 1500 ]; do
          printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"message-1","delta":"tick"}}'
          emitted=$((emitted + 1))
          # The app-server transport admits a bounded burst of unread inbound
          # frames; pace the stream so this case exercises event retention.
          if [ $((emitted % 64)) -eq 0 ]; then sleep 0.01; fi
        done
        printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","completedAtMs":2,"item":{"id":"message-1","type":"agentMessage","text":"done","phase":"final_answer"}}}'
        printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}'
      elif [ "$FAKE_CODEX_MODE" = "primary-silent" ]; then
        printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
        # The completion must not race the turn/start response the client is
        # still admitting, exactly as a real thread would never settle first.
        ( sleep 0.2; printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}' ) &
      elif [ "$FAKE_CODEX_MODE" = "primary" ]; then
        printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
        printf '%s\n' '{"method":"thread/name/updated","params":{"threadId":"thread-1","threadName":"Investigate the failing suite"}}'
        printf '%s\n' '{"method":"mcpServer/startupStatus/updated","params":{"name":"local-tools","status":"ready"}}'
        printf '%s\n' '{"method":"skills/changed","params":{}}'
        printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"message-1","delta":"working"}}'
        printf '%s\n' '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"reasoning-1","summaryIndex":0,"delta":"considering"}}'
        printf '%s\n' '{"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"command-1","startedAtMs":1,"command":"cargo test","reason":"Run tests"}}'
      else
        printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-fork","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
      fi
      ;;
    *'"id":"tool-1","result"'*)
      if [ "$FAKE_CODEX_MODE" != "primary-dynamic-block" ]; then
        printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","completedAtMs":2,"item":{"id":"message-1","type":"agentMessage","text":"done","phase":"final_answer"}}}'
        printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}'
      fi
      ;;
    *'"method":"turn/steer"'*)
      printf '%s\n' '{"id":3,"result":{"turnId":"turn-1"}}'
      ;;
    *'"id":"approval-1","result"'*)
      printf '%s\n' '{"id":"question-1","method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"question-item","questions":[{"id":"confirm","header":"Confirm","question":"Run the full suite?","isSecret":false,"options":[{"label":"Yes","description":"Run it"},{"label":"No","description":"Skip it"}]},{"id":"scope","header":"Coverage","question":"Which checks?","multiSelect":true,"options":[{"label":"Unit","description":"Fast checks"},{"label":"Integration","description":"Real boundaries"}]},{"id":"note","question":"Any additional context?","options":[]}]}}'
      ;;
    *'"id":"question-1","result"'*)
      printf '%s\n' '{"method":"thread/tokenUsage/updated","params":{"threadId":"thread-1","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":11,"cachedInputTokens":0,"outputTokens":7,"reasoningOutputTokens":2,"totalTokens":18},"total":{"inputTokens":11,"cachedInputTokens":0,"outputTokens":7,"reasoningOutputTokens":2,"totalTokens":18}}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","completedAtMs":2,"item":{"id":"message-1","type":"agentMessage","text":"done","phase":"final_answer"}}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}'
      ;;
    *'"method":"thread/compact/start"'*)
      printf '%s\n' '{"id":4,"result":{}}'
      printf '%s\n' '{"method":"thread/compacted","params":{"threadId":"thread-1","turnId":"compact-1"}}'
      ;;
    *'"method":"turn/interrupt"'*)
      printf '%s\n' '{"id":3,"result":{}}'
      if [ "$FAKE_CODEX_MODE" = "primary-dynamic-block" ]; then
        printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"interrupted"}}}'
      else
        printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-fork","turn":{"id":"turn-1","items":[],"status":"interrupted"}}}'
      fi
      ;;
    *'"method":"thread/settings/update"'*)
      printf '%s\n' '{"id":2,"result":{}}'
      ;;
    *'"method":"probe/first"'*) ;;
    *'"method":"probe/second"'*)
      printf '%s\n' '{"id":2,"result":{"name":"second"}}'
      printf '%s\n' '{"id":1,"result":{"name":"first"}}'
      ;;
    *'"method":"probe/error"'*)
      printf '%s\n' '{"id":3,"error":{"code":-32000,"message":"remote-secret-canary"}}'
      ;;
    *'"method":"probe/arm"'*)
      (sleep 0.25; printf survived > "$FAKE_CODEX_MARKER") &
      printf '%s\n' '{"id":4,"result":{}}'
      ;;
    *'"method":"probe/slow"'*) ;;
    *'"method":"probe/release"'*)
      printf '%s\n' '{"id":1,"result":{"late":true}}'
      printf '%s\n' '{"id":2,"result":{"released":true}}'
      ;;
  esac
done
wait
"#
    }
}

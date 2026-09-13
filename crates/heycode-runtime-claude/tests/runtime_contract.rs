//! Claude Code process-provider contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_runtime_claude::{ClaudeCliVersion, ClaudeVersionPolicy};

#[test]
fn supported_version_policy_is_pinned_to_the_verified_major_line() {
    let policy = ClaudeVersionPolicy::supported();
    assert!(policy.accepts(ClaudeCliVersion::new(2, 1, 241)));
    assert!(policy.accepts(ClaudeCliVersion::new(2, 1, 243)));
    assert!(!policy.accepts(ClaudeCliVersion::new(2, 1, 240)));
    assert!(!policy.accepts(ClaudeCliVersion::new(3, 0, 0)));
}

#[tokio::test]
async fn unavailable_optional_executable_does_not_prevent_runtime_construction() {
    use heycode_runtime::{AgentRuntime as _, RuntimeErrorCode};
    use heycode_runtime_claude::{ClaudeRuntime, ClaudeRuntimeConfig};
    use tokio_util::sync::CancellationToken;

    let config = ClaudeRuntimeConfig::new(std::env::current_dir().unwrap())
        .unwrap()
        .with_program("heycode-claude-runtime-deliberately-missing-7f2e")
        .unwrap();
    let runtime = ClaudeRuntime::new(heycode_exec::SubprocessService::local(), config).unwrap();
    assert_eq!(
        runtime
            .account(CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        RuntimeErrorCode::Unavailable
    );
}

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use heycode_exec::{
        SandboxMode, SandboxService, SubprocessService, local_subprocess_plugin,
        sandbox_service_plugin,
    };
    use heycode_runtime::{
        AccountStatus, AgentRuntime, AgentRuntimeKind, AgentRuntimeRegistry, RuntimeErrorCode,
        RuntimeToolCall, RuntimeToolExecutor, RuntimeToolOutput, runtime_registry_plugin,
    };
    use heycode_runtime_claude::{
        CLAUDE_RUNTIME_ID, ClaudeCliVersion, ClaudeRuntime, ClaudeRuntimeConfig,
        claude_runtime_plugin,
    };
    use tokio_util::sync::CancellationToken;

    const PRIVATE_BODY_CANARY: &str = "private-provider-body-canary";

    struct FixtureToolExecutor {
        calls: Mutex<Vec<RuntimeToolCall>>,
        block: bool,
        fail: bool,
        entered: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl RuntimeToolExecutor for FixtureToolExecutor {
        async fn execute(
            &self,
            call: RuntimeToolCall,
            cancellation: CancellationToken,
        ) -> Result<RuntimeToolOutput, heycode_runtime::RuntimeError> {
            self.calls.lock().unwrap().push(call);
            self.entered.notify_one();
            if self.block {
                cancellation.cancelled().await;
                return Ok(RuntimeToolOutput {
                    content: "cancelled".to_owned(),
                    is_error: true,
                });
            }
            if self.fail {
                return Err(heycode_runtime::RuntimeError::unavailable());
            }
            Ok(RuntimeToolOutput {
                content: "host-result".to_owned(),
                is_error: false,
            })
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        program: PathBuf,
        args: PathBuf,
        version_args: PathBuf,
        auth_args: PathBuf,
        stdin_log: PathBuf,
        ready: PathBuf,
        survival: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let program = root.path().join("claude-fixture");
            let args = root.path().join("args");
            let version_args = root.path().join("version-args");
            let auth_args = root.path().join("auth-args");
            let stdin_log = root.path().join("stdin-log");
            let ready = root.path().join("descendant-ready");
            let survival = root.path().join("descendant-survived");
            fs::write(&program, fixture_script()).unwrap();
            let mut permissions = fs::metadata(&program).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&program, permissions).unwrap();
            warm(&program, root.path());
            Self {
                root,
                program,
                stdin_log,
                args,
                version_args,
                auth_args,
                ready,
                survival,
            }
        }

        fn config(&self, mode: &str, version: &str, authenticated: bool) -> ClaudeRuntimeConfig {
            self.config_with(mode, version, authenticated, Vec::new())
        }

        /// A session config whose fixture streams `deltas` text deltas and
        /// then settles with either a textual or a textless success result.
        fn stream_config(&self, deltas: usize, result: &str) -> ClaudeRuntimeConfig {
            self.config_with(
                "stream",
                "2.1.250",
                true,
                vec![
                    (
                        OsString::from("HEYCODE_FIXTURE_DELTAS"),
                        OsString::from(deltas.to_string()),
                    ),
                    (
                        OsString::from("HEYCODE_FIXTURE_RESULT"),
                        OsString::from(result),
                    ),
                ],
            )
        }

        fn config_with(
            &self,
            mode: &str,
            version: &str,
            authenticated: bool,
            extra: Vec<(OsString, OsString)>,
        ) -> ClaudeRuntimeConfig {
            ClaudeRuntimeConfig::new(self.root.path().canonicalize().unwrap())
                .unwrap()
                .with_program(self.program.clone())
                .unwrap()
                .with_environment(
                    [
                        (OsString::from("HEYCODE_FIXTURE_MODE"), OsString::from(mode)),
                        (
                            OsString::from("HEYCODE_FIXTURE_VERSION"),
                            OsString::from(version),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_AUTH"),
                            OsString::from(if authenticated { "true" } else { "false" }),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_ARGS"),
                            self.args.clone().into_os_string(),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_VERSION_ARGS"),
                            self.version_args.clone().into_os_string(),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_AUTH_ARGS"),
                            self.auth_args.clone().into_os_string(),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_STDIN"),
                            self.stdin_log.clone().into_os_string(),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_SESSION"),
                            OsString::from("11111111-2222-4333-8444-555555555555"),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_READY"),
                            self.ready.clone().into_os_string(),
                        ),
                        (
                            OsString::from("HEYCODE_FIXTURE_SURVIVAL"),
                            self.survival.clone().into_os_string(),
                        ),
                    ]
                    .into_iter()
                    .chain(extra),
                )
                .unwrap()
        }
    }

    #[tokio::test]
    async fn plugin_registers_real_delegated_runtime_and_disposes_effects() {
        let fixture = Fixture::new();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let plugins = vec![
            runtime_registry_plugin(),
            sandbox_service_plugin(
                SandboxService::new(SandboxMode::Off, &workspace, None).unwrap(),
            ),
            local_subprocess_plugin(),
            claude_runtime_plugin(fixture.config("success", "2.1.243", true)),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let runtimes = context
            .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
            .unwrap();
        assert_eq!(runtimes.ids().unwrap(), vec![CLAUDE_RUNTIME_ID]);
        let descriptor = runtimes.descriptors().unwrap().remove(0);
        assert_eq!(descriptor.kind(), AgentRuntimeKind::Delegated);
        assert_eq!(descriptor.display_name(), "Claude Code");
        // Runtime model discovery is initialize-probed; session controls are
        // independently implemented by the long-lived stream-json bridge.
        assert!(descriptor.capabilities().models.is_supported());
        for supported in [
            descriptor.capabilities().resume,
            descriptor.capabilities().fork,
            descriptor.capabilities().steer,
            descriptor.capabilities().follow_up,
            descriptor.capabilities().permissions,
            descriptor.capabilities().questions,
            descriptor.capabilities().compaction,
        ] {
            assert!(supported.is_supported());
        }

        let runtime = runtimes.get(CLAUDE_RUNTIME_ID).unwrap().unwrap();
        let account = runtime.account(CancellationToken::new()).await.unwrap();
        assert_eq!(account.status(), AccountStatus::Connected);
        assert_eq!(account.label(), None);
        assert!(!format!("{account:?}").contains("private-account-canary"));
        assert_eq!(
            fs::read_to_string(&fixture.version_args)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["<--version>"]
        );
        assert_eq!(
            fs::read_to_string(&fixture.auth_args)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["<auth>", "<status>", "<--json>"]
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            runtime.models(cancelled).await.unwrap_err().code(),
            RuntimeErrorCode::Cancelled
        );

        context.shutdown();
        assert!(runtimes.ids().unwrap().is_empty());
        assert_eq!(
            runtime
                .account(CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            RuntimeErrorCode::Closed
        );
    }

    #[tokio::test]
    async fn exact_query_argv_selects_safe_tool_free_no_persistence_mode() {
        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            SubprocessService::local(),
            fixture.config("success", "2.1.243", true),
        )
        .unwrap();
        let receipt = runtime.handshake(CancellationToken::new()).await.unwrap();
        assert_eq!(receipt.version(), ClaudeCliVersion::new(2, 1, 243));
        assert!(receipt.no_session_persistence());

        let arguments = fs::read_to_string(&fixture.args).unwrap();
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            vec![
                "<--safe-mode>",
                "<--strict-mcp-config>",
                "<--mcp-config>",
                "<{\"mcpServers\":{}}>",
                "<--tools>",
                "<>",
                "<--disable-slash-commands>",
                "<--no-chrome>",
                "<--permission-mode>",
                "<dontAsk>",
                "<--no-session-persistence>",
                "<--output-format>",
                "<stream-json>",
                "<--verbose>",
                "<--prompt-suggestions>",
                "<false>",
                "<--system-prompt>",
                "<Return only the exact requested handshake token. Do not inspect files or use tools.>",
                "<--print>",
                "<Reply exactly R07_CLAUDE_HANDSHAKE_OK.>",
            ]
        );
        runtime.close(CancellationToken::new()).await.unwrap();
        runtime.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn disconnected_status_is_a_safe_state_not_a_process_failure() {
        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            SubprocessService::local(),
            fixture.config("success", "2.1.243", false),
        )
        .unwrap();
        let account = runtime.account(CancellationToken::new()).await.unwrap();
        assert_eq!(account.status(), AccountStatus::Disconnected);
        assert_eq!(account.label(), None);
        assert_eq!(
            runtime
                .handshake(CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            RuntimeErrorCode::Unauthorized
        );
        assert!(!fixture.args.exists());
    }

    #[tokio::test]
    async fn incompatible_version_and_malformed_output_are_body_free() {
        let incompatible_fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            SubprocessService::local(),
            incompatible_fixture.config("success", "3.0.0", true),
        )
        .unwrap();
        let error = runtime
            .handshake(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), RuntimeErrorCode::Unavailable);
        assert!(!incompatible_fixture.args.exists());

        let malformed_fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            SubprocessService::local(),
            malformed_fixture.config("malformed", "2.1.243", true),
        )
        .unwrap();
        let error = runtime
            .handshake(CancellationToken::new())
            .await
            .unwrap_err();
        // Deliberately NOT asserting the error class here. This drives a real
        // process, so under load the handshake can time out and legitimately
        // report `Unavailable` instead of `Protocol` — a fact about the machine,
        // not the parser. The classification is pinned deterministically by
        // `malformed_handshake_output_is_a_protocol_error_and_keeps_the_body_out`.
        // What must hold on EVERY path, fast or slow, is that no provider body
        // escapes.
        assert!(!error.to_string().contains(PRIVATE_BODY_CANARY));
        assert!(!format!("{error:?}").contains(PRIVATE_BODY_CANARY));
    }

    #[tokio::test]
    async fn executable_replacement_between_version_and_auth_fails_closed() {
        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            SubprocessService::local(),
            fixture.config("replace", "2.1.243", true),
        )
        .unwrap();
        let error = runtime
            .handshake(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), RuntimeErrorCode::Unavailable);
        assert!(!fixture.auth_args.exists());
        assert!(!fixture.args.exists());
        assert!(
            !format!("{error:?} {error}").contains(fixture.root.path().to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn quiescent_close_cancels_and_reaps_the_private_process_tree() {
        let fixture = Fixture::new();
        let runtime = Arc::new(
            ClaudeRuntime::new(
                SubprocessService::local(),
                fixture.config("hang", "2.1.243", true),
            )
            .unwrap(),
        );
        let query = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.handshake(CancellationToken::new()).await })
        };
        wait_for_file(&fixture.ready).await;
        runtime.close(CancellationToken::new()).await.unwrap();
        let error = query.await.unwrap().unwrap_err();
        assert_eq!(error.code(), RuntimeErrorCode::Closed);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !fixture.survival.exists(),
            "Claude runtime close returned before its descendant tree settled"
        );
        runtime.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn caller_cancellation_settles_the_private_process_tree() {
        let fixture = Fixture::new();
        let runtime = Arc::new(
            ClaudeRuntime::new(
                SubprocessService::local(),
                fixture.config("hang", "2.1.243", true),
            )
            .unwrap(),
        );
        let cancellation = CancellationToken::new();
        let query = {
            let runtime = runtime.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move { runtime.handshake(cancellation).await })
        };
        wait_for_file(&fixture.ready).await;
        cancellation.cancel();
        let error = query.await.unwrap().unwrap_err();
        assert_eq!(error.code(), RuntimeErrorCode::Cancelled);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(!fixture.survival.exists());
        runtime.close(CancellationToken::new()).await.unwrap();
    }

    /// R02: a single turn longer than the retained event window must still
    /// settle, and a subscriber that attaches later must still receive a
    /// well-formed stream that begins at session-ready.
    #[tokio::test]
    async fn a_turn_longer_than_the_retained_window_settles_and_stays_subscribable() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeEventKind, RuntimeFinishReason, RuntimeInput, RuntimeStart};

        const DELTAS: usize = 1_500;

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.stream_config(DELTAS, "done"),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let start = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
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
        let mut final_text = None;
        let reason = loop {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::CommentaryDelta { .. } => deltas += 1,
                RuntimeEventKind::FinalMessage { text } => final_text = Some(text.clone()),
                RuntimeEventKind::TurnFinished { reason, .. } => break *reason,
                _ => {}
            }
        };
        assert_eq!(reason, RuntimeFinishReason::Stop);
        assert_eq!(deltas, DELTAS);
        assert_eq!(final_text.as_deref(), Some("done"));

        // The retained window necessarily dropped earlier events, so this is
        // the case a verbatim replay could not keep valid.
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
            replayed <= DELTAS,
            "the retained window must stay bounded, replayed {replayed}"
        );
        session.close(CancellationToken::new()).await.unwrap();
    }

    /// Claude reports `result` as either a string or a content-block array;
    /// the array shape must project its text instead of arriving empty.
    #[tokio::test]
    async fn an_array_shaped_success_result_projects_its_text() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeEventKind, RuntimeInput, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.stream_config(0, "array"),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let start = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
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
                RuntimeInput::new("answer in blocks").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut final_text = None;
        loop {
            let event = events.next().await.unwrap().unwrap();
            match event.kind() {
                RuntimeEventKind::FinalMessage { text } => final_text = Some(text.clone()),
                RuntimeEventKind::TurnFinished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(final_text.as_deref(), Some("done"));
        session.close(CancellationToken::new()).await.unwrap();
    }

    /// A `success` result carrying no text is still a stopped turn, so the
    /// adapter must publish the final message R02 requires rather than leaving
    /// a violation in the session's replay forever.
    #[tokio::test]
    async fn a_textless_success_result_still_publishes_a_final_message() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeEventKind, RuntimeFinishReason, RuntimeInput, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.stream_config(0, "none"),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let start = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
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
                RuntimeInput::new("answer with no text").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut final_text = None;
        let reason = loop {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::FinalMessage { text } => final_text = Some(text.clone()),
                RuntimeEventKind::TurnFinished { reason, .. } => break *reason,
                _ => {}
            }
        };
        assert_eq!(reason, RuntimeFinishReason::Stop);
        assert_eq!(final_text.as_deref(), Some(""));

        // The whole point of the defect was permanence: a later subscriber
        // replays the same history and must not inherit a violation.
        let mut late = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            late.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));
        loop {
            let event = late.next().await.unwrap().unwrap();
            if matches!(event.kind(), RuntimeEventKind::TurnFinished { .. }) {
                break;
            }
        }
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn initial_configuration_and_sdk_mcp_tool_roundtrip_are_exact() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeConfiguration, RuntimeEventKind, RuntimeInput, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("mcp", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let executor = Arc::new(FixtureToolExecutor {
            calls: Mutex::new(Vec::new()),
            block: false,
            fail: false,
            entered: tokio::sync::Notify::new(),
        });
        let configuration = RuntimeConfiguration::new()
            .with_system_prompt("exact Claude instructions")
            .unwrap()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo one value".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            }])
            .unwrap()
            .with_model("claude-fable-5")
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap();
        let start = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
            .unwrap()
            .with_configuration(configuration)
            .with_tool_executor(executor.clone());
        let session = runtime
            .start(start, CancellationToken::new())
            .await
            .unwrap();
        let argv = fs::read_to_string(&fixture.args).unwrap();
        assert!(argv.contains("<--system-prompt>\n<exact Claude instructions>"));
        assert!(argv.contains("<--model>\n<claude-fable-5>"));
        assert!(argv.contains("<--effort>\n<high>"));
        for argument in [
            "--safe-mode",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-chrome",
        ] {
            assert!(
                argv.contains(&format!("<{argument}>")),
                "missing {argument}"
            );
        }
        assert!(argv.contains("<--mcp-config>\n<{\"mcpServers\":{}}>"));
        assert!(argv.contains("<--permission-mode>\n<manual>"));
        assert!(argv.contains("<--permission-prompts>\n<host>"));
        assert!(argv.contains("<--permission-prompt-tool>\n<stdio>"));
        assert!(argv.contains("<--tools>\n<>"));
        assert!(argv.contains("<--allowedTools>\n<mcp__heycode__echo>"));
        let stdin = fs::read_to_string(&fixture.stdin_log).unwrap();
        assert!(stdin.contains(r#""sdkMcpServers":["heycode"]"#));

        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        assert!(matches!(
            events.next().await.unwrap().unwrap().kind(),
            RuntimeEventKind::SessionReady
        ));
        session
            .send(
                RuntimeInput::new("call echo").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut calls = Vec::new();
        let mut results = Vec::new();
        loop {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => calls.push((call_id.as_str().to_owned(), name.clone(), arguments.clone())),
                RuntimeEventKind::ToolResult {
                    call_id, is_error, ..
                } => results.push((call_id.as_str().to_owned(), *is_error)),
                RuntimeEventKind::TurnFinished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.starts_with("claude-mcp-"));
        assert_eq!(calls[0].1, "echo");
        assert_eq!(calls[0].2, serde_json::json!({"text":"hello"}));
        assert_eq!(results, [(calls[0].0.clone(), false)]);
        let stdin = fs::read_to_string(&fixture.stdin_log).unwrap();
        assert!(stdin.contains(
            r#""request_id":"mcp-notify","response":{"mcp_response":{"id":0,"jsonrpc":"2.0","result":{}}}"#
        ));
        {
            let executed = executor.calls.lock().unwrap();
            assert_eq!(executed.len(), 1);
            assert_eq!(executed[0].name, "echo");
        }
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn sdk_mcp_json_rpc_id_reuse_gets_unique_runtime_call_ids() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeConfiguration, RuntimeEventKind, RuntimeInput, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("mcp-repeat", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let executor = Arc::new(FixtureToolExecutor {
            calls: Mutex::new(Vec::new()),
            block: false,
            fail: false,
            entered: tokio::sync::Notify::new(),
        });
        let configuration = RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo one value".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            }])
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
                    .unwrap()
                    .with_configuration(configuration)
                    .with_tool_executor(executor.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        session
            .send(
                RuntimeInput::new("call echo twice").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut calls = Vec::new();
        while let Some(event) = events.next().await {
            match event.unwrap().kind() {
                RuntimeEventKind::ToolCall {
                    call_id, arguments, ..
                } => calls.push((call_id.as_str().to_owned(), arguments.clone())),
                RuntimeEventKind::TurnFinished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].0, calls[1].0);
        assert!(calls.iter().all(|(id, _)| id.starts_with("claude-mcp-")));
        assert_eq!(calls[0].1, serde_json::json!({"text":"hello"}));
        assert_eq!(calls[1].1, serde_json::json!({"text":"again"}));
        assert_eq!(executor.calls.lock().unwrap().len(), 2);
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn live_configuration_applies_provider_controls_and_rejects_launch_only_fields() {
        use heycode_runtime::{RuntimeConfiguration, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("session", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let before = fs::read_to_string(&fixture.stdin_log).unwrap();
        let update = RuntimeConfiguration::new()
            .with_system_prompt("cannot change live")
            .unwrap()
            .with_tools(Vec::new())
            .unwrap();
        let error = session
            .configure(update, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), RuntimeErrorCode::Unsupported);
        assert_eq!(
            error.message(),
            "runtime configuration fields are unsupported: system_prompt, tools"
        );
        assert_eq!(fs::read_to_string(&fixture.stdin_log).unwrap(), before);

        // Effort values belong to the provider catalog rather than a heycode
        // global enum. The bridge forwards the value exactly through the
        // official session-scoped flag-settings control.
        let effective = session
            .configure(
                RuntimeConfiguration::new()
                    .with_model("claude-fable-5.1")
                    .unwrap()
                    .with_reasoning_effort("provider-native")
                    .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(effective.model(), Some("claude-fable-5.1"));
        assert_eq!(effective.reasoning_effort(), Some("provider-native"));
        let stdin = fs::read_to_string(&fixture.stdin_log).unwrap();
        let controls = stdin
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|frame| frame.get("request").cloned())
            .collect::<Vec<_>>();
        assert!(controls.iter().any(|request| {
            request["subtype"] == "set_model" && request["model"] == "claude-fable-5.1"
        }));
        assert!(controls.iter().any(|request| {
            request["subtype"] == "apply_flag_settings"
                && request["settings"]["effortLevel"] == "provider-native"
        }));
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn sdk_mcp_rejects_unconfigured_tool_names_without_killing_the_session() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeConfiguration, RuntimeEventKind, RuntimeInput, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("mcp-unknown", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let executor = Arc::new(FixtureToolExecutor {
            calls: Mutex::new(Vec::new()),
            block: false,
            fail: false,
            entered: tokio::sync::Notify::new(),
        });
        let configuration = RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo one value".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            }])
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
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
        loop {
            let event = events.next().await.unwrap().unwrap();
            if matches!(event.kind(), RuntimeEventKind::TurnFinished { .. }) {
                break;
            }
        }
        assert!(executor.calls.lock().unwrap().is_empty());
        let stdin = fs::read_to_string(&fixture.stdin_log).unwrap();
        assert!(stdin.contains(r#""code":-32602,"message":"MCP tool is not available""#));
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn sdk_mcp_executor_failure_is_a_correlated_tool_error() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeConfiguration, RuntimeEventKind, RuntimeInput, RuntimeStart};

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("mcp-executor-error", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let executor = Arc::new(FixtureToolExecutor {
            calls: Mutex::new(Vec::new()),
            block: false,
            fail: true,
            entered: tokio::sync::Notify::new(),
        });
        let configuration = RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo one value".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            }])
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
                    .unwrap()
                    .with_configuration(configuration)
                    .with_tool_executor(executor.clone()),
                CancellationToken::new(),
            )
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
        let mut call = None;
        let mut result = None;
        loop {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::ToolCall { call_id, .. } => {
                    call = Some(call_id.as_str().to_owned());
                }
                RuntimeEventKind::ToolResult {
                    call_id, is_error, ..
                } => result = Some((call_id.as_str().to_owned(), *is_error)),
                RuntimeEventKind::TurnFinished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(result, call.map(|call_id| (call_id, true)));
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
        let stdin = fs::read_to_string(&fixture.stdin_log).unwrap();
        assert!(stdin.contains(r#""text":"host tool execution failed""#));
        assert!(stdin.contains(r#""isError":true"#));
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_turn_retracts_an_active_sdk_mcp_call() {
        use futures::StreamExt as _;
        use heycode_runtime::{
            RuntimeConfiguration, RuntimeEventKind, RuntimeFinishReason, RuntimeInput, RuntimeStart,
        };

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("mcp-block", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let executor = Arc::new(FixtureToolExecutor {
            calls: Mutex::new(Vec::new()),
            block: true,
            fail: false,
            entered: tokio::sync::Notify::new(),
        });
        let configuration = RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "echo".to_owned(),
                description: "Echo one value".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            }])
            .unwrap();
        let session = runtime
            .start(
                RuntimeStart::new(heycode_core::SessionId::generate(), &workspace)
                    .unwrap()
                    .with_configuration(configuration)
                    .with_tool_executor(executor.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
        session
            .send(
                RuntimeInput::new("call and block").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), executor.entered.notified())
            .await
            .expect("fixture tool never started");
        tokio::time::timeout(
            Duration::from_secs(1),
            session.cancel(CancellationToken::new()),
        )
        .await
        .expect("turn cancellation did not retract the MCP call")
        .unwrap();
        let reason = loop {
            let event = events.next().await.unwrap().unwrap();
            if let RuntimeEventKind::TurnFinished { reason, .. } = event.kind() {
                break *reason;
            }
        };
        assert_eq!(reason, RuntimeFinishReason::Cancelled);
        session.close(CancellationToken::new()).await.unwrap();
    }

    #[tokio::test]
    async fn vendor_cancellation_and_eof_remain_observable_during_host_approval() {
        use futures::StreamExt as _;
        use heycode_runtime::{RuntimeConfiguration, RuntimeEventKind, RuntimeInput, RuntimeStart};
        for mode in ["mcp-vendor-cancel", "mcp-eof"] {
            let fixture = Fixture::new();
            let runtime = ClaudeRuntime::new(
                heycode_exec::SubprocessService::local(),
                fixture.config(mode, "2.1.250", true),
            )
            .unwrap();
            let executor = Arc::new(FixtureToolExecutor {
                calls: Mutex::new(Vec::new()),
                block: true,
                fail: false,
                entered: tokio::sync::Notify::new(),
            });
            let configuration = RuntimeConfiguration::new()
                .with_tools(vec![heycode_core::ToolSpec {
                    name: "echo".into(),
                    description: "Echo".into(),
                    parameters: serde_json::json!({"type":"object"}),
                }])
                .unwrap();
            let session = runtime
                .start(
                    RuntimeStart::new(
                        heycode_core::SessionId::generate(),
                        fixture.root.path().canonicalize().unwrap(),
                    )
                    .unwrap()
                    .with_configuration(configuration)
                    .with_tool_executor(executor.clone()),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let mut events = heycode_runtime::normalize_runtime_event_stream(session.subscribe());
            session
                .send(
                    RuntimeInput::new("wait for approval").unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), executor.entered.notified())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                let mut cancelled_result = false;
                loop {
                    match events.next().await.unwrap() {
                        Err(error) => {
                            assert_eq!(mode, "mcp-eof");
                            assert_eq!(error.code(), RuntimeErrorCode::Unavailable);
                            break;
                        }
                        Ok(event) => match event.kind() {
                            RuntimeEventKind::ToolResult {
                                result,
                                is_error: true,
                                ..
                            } => {
                                assert_eq!(result.as_str(), Some("cancelled"));
                                cancelled_result = true;
                            }
                            RuntimeEventKind::TurnFinished { .. } => {
                                assert_eq!(mode, "mcp-vendor-cancel");
                                assert!(cancelled_result);
                                break;
                            }
                            _ => {}
                        },
                    }
                }
            })
            .await
            .expect("stream reader blocked behind human approval");
            tokio::time::timeout(
                Duration::from_secs(2),
                session.close(CancellationToken::new()),
            )
            .await
            .expect("session did not drain its owned tool future")
            .unwrap();
        }
    }

    async fn wait_for_file(path: &Path) {
        for _ in 0..250 {
            if path.is_file() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("Claude fixture descendant readiness marker was not created");
    }

    /// R09 acceptance: resume/fork, partial events, compaction and questions.
    #[tokio::test]
    async fn primary_session_streams_answers_permission_and_question_then_compacts() {
        use futures::StreamExt as _;
        use heycode_runtime::{
            RuntimeCompactOutcome, RuntimeEventKind, RuntimeFinishReason, RuntimeInput,
            RuntimePermissionDecision, RuntimePermissionResponse, RuntimeQuestionResponse,
            RuntimeStart,
        };

        let fixture = Fixture::new();
        let runtime = ClaudeRuntime::new(
            heycode_exec::SubprocessService::local(),
            fixture.config("session", "2.1.250", true),
        )
        .unwrap();
        let workspace = fixture.root.path().canonicalize().unwrap();
        let start = RuntimeStart::new(heycode_core::SessionId::generate(), &workspace).unwrap();
        let session = runtime
            .start(start, CancellationToken::new())
            .await
            .unwrap();
        // The host chooses the identity so `system/init` can be verified against
        // an expected value instead of trusted blindly.
        let argv = fs::read_to_string(&fixture.args).unwrap();
        assert!(
            argv.contains(&format!("<--session-id>\n<{}>", session.id().as_str())),
            "the reported id must be the exact host-chosen one"
        );
        assert!(!argv.contains("<--resume>"));

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

        let mut permission = None;
        let mut saw_commentary = false;
        let mut saw_reasoning = false;
        while permission.is_none() {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::CommentaryDelta { text } if text == "working" => {
                    saw_commentary = true;
                }
                RuntimeEventKind::ReasoningDelta { text } if text == "planning" => {
                    saw_reasoning = true;
                }
                RuntimeEventKind::PermissionRequested {
                    request_id,
                    action,
                    detail,
                } => {
                    assert!(action.contains("Bash"));
                    assert_eq!(
                        detail,
                        r#"writes files; input: {"command":"ls -la","timeout_ms":1000}"#
                    );
                    permission = Some(request_id.clone());
                }
                _ => {}
            }
        }
        assert!(
            saw_commentary && saw_reasoning,
            "partial events must stream"
        );

        // A follow-up appends without starting a turn while one is running.
        session
            .follow_up(
                RuntimeInput::new("also check lints").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        session
            .respond_permission(
                RuntimePermissionResponse::new(
                    permission.clone().unwrap(),
                    RuntimePermissionDecision::AllowOnce,
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // The same request cannot be answered twice.
        assert!(
            session
                .respond_permission(
                    RuntimePermissionResponse::new(
                        permission.unwrap(),
                        RuntimePermissionDecision::Deny,
                    ),
                    CancellationToken::new(),
                )
                .await
                .is_err()
        );

        let question = loop {
            if let RuntimeEventKind::QuestionRequested {
                request_id,
                header,
                prompt,
                choices,
                choice_descriptions,
                ..
            } = events.next().await.unwrap().unwrap().kind()
            {
                assert_eq!(header.as_deref(), Some("Intent"));
                assert_eq!(prompt, "Run the full suite?");
                assert_eq!(choices, &["Yes", "No"]);
                assert_eq!(
                    choice_descriptions,
                    &[
                        Some("Run all checks now".to_owned()),
                        Some("Skip the full suite".to_owned()),
                    ]
                );
                break request_id.clone();
            }
        };
        session
            .respond_question(
                RuntimeQuestionResponse::new(question, "Yes").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut calls = Vec::new();
        let mut results = Vec::new();
        let mut saw_final = false;
        let mut usage = None;
        loop {
            match events.next().await.unwrap().unwrap().kind() {
                RuntimeEventKind::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    calls.push((call_id.as_str().to_owned(), name.clone(), arguments.clone()));
                }
                RuntimeEventKind::ToolResult { call_id, .. } => {
                    results.push(call_id.as_str().to_owned());
                }
                RuntimeEventKind::FinalMessage { text } if text == "done" => saw_final = true,
                RuntimeEventKind::Usage { usage: seen, .. } => usage = Some(*seen),
                RuntimeEventKind::TurnFinished {
                    turn: finished,
                    reason,
                } => {
                    assert_eq!(finished, &turn);
                    assert_eq!(*reason, RuntimeFinishReason::Stop);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_final);
        // A nested subagent frame must never correlate against this session.
        assert_eq!(
            calls,
            vec![(
                "toolu_1".to_owned(),
                "Bash".to_owned(),
                serde_json::json!({"command":"ls -la","timeout_ms":1000}),
            )]
        );
        assert_eq!(results, vec!["toolu_1".to_owned()]);
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 15, "cache reads fold into prompt");
        assert_eq!(usage.completion_tokens, 7);

        assert_eq!(
            session.compact(CancellationToken::new()).await.unwrap(),
            RuntimeCompactOutcome::Applied
        );
        session.close(CancellationToken::new()).await.unwrap();

        let stdin = fs::read_to_string(&fixture.stdin_log).unwrap();
        assert!(stdin.contains(r#""text":"do the work""#));
        assert!(
            stdin.contains(r#""session_id":"""#),
            "the official transport lets the CLI own session identity"
        );
        assert!(!stdin.contains(r#""shouldQuery":true"#));
        assert!(
            stdin.contains(r#""shouldQuery":false"#),
            "follow-up must not query"
        );
        assert!(stdin.contains(r#""behavior":"allow""#));
        assert!(stdin.contains(r#""updatedInput":{"command":"ls -la","timeout_ms":1000}"#));
        // A persistent settings write is broader authority than heycode grants.
        assert!(!stdin.contains("updatedPermissions"));
        assert!(stdin.contains(r#""response":"Yes""#));
        assert!(stdin.contains(r#""text":"/compact""#));
    }

    #[tokio::test]
    async fn resume_and_fork_bind_the_pinned_identity_flags() {
        use heycode_runtime::{RuntimeFork, RuntimeResume, RuntimeSessionId};

        for (label, expect_fork) in [("resume", false), ("fork", true)] {
            let fixture = Fixture::new();
            let runtime = ClaudeRuntime::new(
                heycode_exec::SubprocessService::local(),
                fixture.config("session", "2.1.250", true),
            )
            .unwrap();
            let workspace = fixture.root.path().canonicalize().unwrap();
            let source = RuntimeSessionId::new("11111111-2222-4333-8444-555555555555").unwrap();
            let session = if expect_fork {
                let request =
                    RuntimeFork::new(heycode_core::SessionId::generate(), &workspace, source)
                        .unwrap();
                runtime
                    .fork(request, CancellationToken::new())
                    .await
                    .unwrap()
            } else {
                let request =
                    RuntimeResume::new(heycode_core::SessionId::generate(), &workspace, source)
                        .unwrap();
                runtime
                    .resume(request, CancellationToken::new())
                    .await
                    .unwrap()
            };
            let argv = fs::read_to_string(&fixture.args).unwrap();
            assert!(
                argv.contains("<--resume>\n<11111111-2222-4333-8444-555555555555>"),
                "{label} must resume the exact source id"
            );
            assert_eq!(
                argv.contains("<--fork-session>"),
                expect_fork,
                "{label} fork flag"
            );
            assert!(
                !argv.contains("<--session-id>"),
                "{label} must not mint an id"
            );
            session.close(CancellationToken::new()).await.unwrap();
        }
    }

    /// Execute the freshly written fixture once, off the clock.
    ///
    /// A newly created executable is scanned on its first execution — measured
    /// at 11-23 seconds on the development machine, against 5ms for the same
    /// file a second time. The runtime's own `--version` budget is 5 seconds,
    /// so without this the test measures the host's scanner rather than the
    /// runtime, and fails for a reason that has nothing to do with heycode.
    ///
    /// Warming is deliberately untimed and its result ignored: the point is
    /// only that the kernel has seen this inode execute once.
    fn warm(program: &std::path::Path, root: &std::path::Path) {
        let scratch = root.join("warm");
        let _ = std::process::Command::new(program)
            .arg("--version")
            .env("HEYCODE_FIXTURE_MODE", "warm")
            .env("HEYCODE_FIXTURE_VERSION", "0.0.0")
            .env("HEYCODE_FIXTURE_VERSION_ARGS", &scratch)
            .env("HEYCODE_FIXTURE_ARGS", &scratch)
            .env("HEYCODE_FIXTURE_AUTH_ARGS", &scratch)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = fs::remove_file(&scratch);
    }

    fn fixture_script() -> &'static [u8] {
        br##"#!/bin/sh
if [ "$1" = "--version" ]; then
  : > "$HEYCODE_FIXTURE_VERSION_ARGS"
  for argument in "$@"; do
    printf '<%s>\n' "$argument" >> "$HEYCODE_FIXTURE_VERSION_ARGS"
  done
  printf '%s (Claude Code)\n' "$HEYCODE_FIXTURE_VERSION"
  if [ "$HEYCODE_FIXTURE_MODE" = "replace" ]; then
    printf '%s\n' '#!/bin/sh' 'exit 0' > "${0}.replacement"
    /bin/mv -f "${0}.replacement" "$0"
  fi
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ] && [ "$3" = "--json" ]; then
  : > "$HEYCODE_FIXTURE_AUTH_ARGS"
  for argument in "$@"; do
    printf '<%s>\n' "$argument" >> "$HEYCODE_FIXTURE_AUTH_ARGS"
  done
  if [ "$HEYCODE_FIXTURE_AUTH" = "true" ]; then
    printf '%s\n' '{"loggedIn":true,"email":"private-account-canary","orgId":"private-org-canary"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"detail":"private-account-canary"}'
  exit 1
fi
: > "$HEYCODE_FIXTURE_ARGS"
for argument in "$@"; do
  printf '<%s>\n' "$argument" >> "$HEYCODE_FIXTURE_ARGS"
done
if [ "$HEYCODE_FIXTURE_MODE" = "malformed" ]; then
  printf '%s\n' 'private-provider-body-canary'
  exit 0
fi
if [ "$HEYCODE_FIXTURE_MODE" = "hang" ]; then
  ( /bin/sleep 0.65; printf '%s' escaped > "$HEYCODE_FIXTURE_SURVIVAL" ) &
  : > "$HEYCODE_FIXTURE_READY"
  /bin/sleep 20
  exit 1
fi
if [ "$HEYCODE_FIXTURE_MODE" = "stream" ]; then
  sid="$HEYCODE_FIXTURE_SESSION"
  previous=""
  for argument in "$@"; do
    case "$previous" in
      --session-id|--resume) sid="$argument" ;;
    esac
    previous="$argument"
  done
  printf '{"type":"system","subtype":"init","session_id":"%s","model":"claude-fable-5","cwd":"/w","tools":[]}\n' "$sid"
  while IFS= read -r line; do
    case "$line" in
      *'"subtype":"initialize"'*)
        rid=$(printf '%s' "$line" | /usr/bin/sed -e 's/.*"request_id":"\([^"]*\)".*/\1/')
        printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$rid"
        ;;
      *'"type":"user"'*)
        emitted=0
        while [ "$emitted" -lt "$HEYCODE_FIXTURE_DELTAS" ]; do
          printf '{"type":"stream_event","session_id":"%s","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"tick"}}}\n' "$sid"
          emitted=$((emitted+1))
        done
        if [ "$HEYCODE_FIXTURE_RESULT" = "none" ]; then
          printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid"
        elif [ "$HEYCODE_FIXTURE_RESULT" = "array" ]; then
          printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":[{"type":"text","text":"done"}],"usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid"
        else
          printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"%s","usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid" "$HEYCODE_FIXTURE_RESULT"
        fi
        ;;
    esac
  done
  exit 0
fi
if [ "${HEYCODE_FIXTURE_MODE#mcp}" != "$HEYCODE_FIXTURE_MODE" ]; then
  sid="$HEYCODE_FIXTURE_SESSION"
  previous=""
  for argument in "$@"; do
    case "$previous" in
      --session-id|--resume) sid="$argument" ;;
    esac
    previous="$argument"
  done
  printf '{"type":"system","subtype":"init","session_id":"%s","model":"claude-fable-5","cwd":"/w","tools":[]}\n' "$sid"
  while IFS= read -r line; do
    printf '%s\n' "$line" >> "$HEYCODE_FIXTURE_STDIN"
    case "$line" in
      *'"subtype":"initialize"'*)
        rid=$(printf '%s' "$line" | /usr/bin/sed -e 's/.*"request_id":"\([^"]*\)".*/\1/')
        printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$rid"
        ;;
      *'"type":"user"'*)
        printf '{"type":"control_request","request_id":"mcp-init","session_id":"%s","request":{"subtype":"mcp_message","server_name":"heycode","message":{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}}}\n' "$sid"
        ;;
      *'"request_id":"mcp-init"'*)
        printf '{"type":"control_request","request_id":"mcp-notify","session_id":"%s","request":{"subtype":"mcp_message","server_name":"heycode","message":{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}}}\n' "$sid"
        ;;
      *'"request_id":"mcp-notify"'*)
        printf '{"type":"control_request","request_id":"mcp-list","session_id":"%s","request":{"subtype":"mcp_message","server_name":"heycode","message":{"jsonrpc":"2.0","id":0,"method":"tools/list","params":{}}}}\n' "$sid"
        ;;
      *'"request_id":"mcp-list"'*)
        tool=echo
        if [ "$HEYCODE_FIXTURE_MODE" = "mcp-unknown" ]; then tool=unknown; fi
        printf '{"type":"assistant","session_id":"%s","parent_tool_use_id":null,"message":{"role":"assistant","content":[{"type":"tool_use","id":"hosted_1","name":"mcp__heycode__%s","input":{"text":"hello"}}]}}\n' "$sid" "$tool"
        printf '{"type":"control_request","request_id":"mcp-call","session_id":"%s","request":{"subtype":"mcp_message","server_name":"heycode","message":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"%s","arguments":{"text":"hello"}}}}}\n' "$sid" "$tool"
        if [ "$HEYCODE_FIXTURE_MODE" = "mcp-vendor-cancel" ]; then
          printf '{"type":"control_cancel_request","request_id":"mcp-call"}\n'
        elif [ "$HEYCODE_FIXTURE_MODE" = "mcp-eof" ]; then
          exit 0
        fi
        ;;
      *'"request_id":"mcp-call-2"'*)
        if [ "$HEYCODE_FIXTURE_MODE" = "mcp-repeat" ]; then
          printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"done","usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid"
        fi
        ;;
      *'"request_id":"mcp-call"'*)
        if [ "$HEYCODE_FIXTURE_MODE" = "mcp" ]; then
          printf '{"type":"user","session_id":"%s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"hosted_1","is_error":false,"content":[{"type":"text","text":"host-result"}]}]}}\n' "$sid"
          printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"done","usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid"
        elif [ "$HEYCODE_FIXTURE_MODE" = "mcp-repeat" ]; then
          printf '{"type":"control_request","request_id":"mcp-call-2","session_id":"%s","request":{"subtype":"mcp_message","server_name":"heycode","message":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"text":"again"}}}}}\n' "$sid"
        elif [ "$HEYCODE_FIXTURE_MODE" = "mcp-unknown" ] || [ "$HEYCODE_FIXTURE_MODE" = "mcp-executor-error" ] || [ "$HEYCODE_FIXTURE_MODE" = "mcp-vendor-cancel" ]; then
          printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"done","usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid"
        fi
        ;;
      *'"subtype":"interrupt"'*)
        rid=$(printf '%s' "$line" | /usr/bin/sed -e 's/.*"request_id":"\([^"]*\)".*/\1/')
        printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$rid"
        printf '{"type":"result","subtype":"error_during_execution","session_id":"%s","is_error":true}\n' "$sid"
        ;;
    esac
  done
  exit 0
fi
if [ "$HEYCODE_FIXTURE_MODE" = "session" ]; then
  # Model the real CLI: --session-id and --resume choose the identity, while
  # --fork-session deliberately reports a NEW one.
  sid="$HEYCODE_FIXTURE_SESSION"
  forked=0
  previous=""
  for argument in "$@"; do
    case "$previous" in
      --session-id|--resume) sid="$argument" ;;
    esac
    if [ "$argument" = "--fork-session" ]; then forked=1; fi
    previous="$argument"
  done
  if [ "$forked" = "1" ]; then sid="99999999-8888-4777-8666-555555555555"; fi
  # Hook and plugin frames legitimately precede init on a real session.
  printf '%s\n' '{"type":"hook_started","session_id":"'"$sid"'"}'
  printf '{"type":"system","subtype":"plugin_install","status":"completed","session_id":"%s"}\n' "$sid"
  printf '{"type":"system","subtype":"init","session_id":"%s","model":"claude-fable-5","cwd":"/w","tools":[]}\n' "$sid"
  while IFS= read -r line; do
    printf '%s\n' "$line" >> "$HEYCODE_FIXTURE_STDIN"
    case "$line" in
      *'"subtype":"initialize"'*)
        rid=$(printf '%s' "$line" | /usr/bin/sed -e 's/.*"request_id":"\([^"]*\)".*/\1/')
        printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$rid"
        ;;
      *'"subtype":"set_model"'*|*'"subtype":"apply_flag_settings"'*)
        rid=$(printf '%s' "$line" | /usr/bin/sed -e 's/.*"request_id":"\([^"]*\)".*/\1/')
        printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$rid"
        ;;
      *'"text":"/compact"'*)
        printf '{"type":"system","subtype":"compact_boundary","session_id":"%s","compact_metadata":{"trigger":"manual","pre_tokens":42}}\n' "$sid"
        printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"compacted","usage":{"input_tokens":1,"output_tokens":1}}\n' "$sid"
        ;;
      *'"subtype":"interrupt"'*)
        rid=$(printf '%s' "$line" | /usr/bin/sed -e 's/.*"request_id":"\([^"]*\)".*/\1/')
        printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$rid"
        printf '{"type":"result","subtype":"error_during_execution","session_id":"%s","is_error":true}\n' "$sid"
        ;;
      *'"shouldQuery":false'*)
        # A follow-up appends without starting a turn: emit nothing.
        ;;
      *'"type":"user"'*)
        printf '%s\n' '{"type":"task_started","session_id":"'"$sid"'","task_id":"t1"}'
        printf '{"type":"stream_event","session_id":"%s","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"planning"}}}\n' "$sid"
        printf '{"type":"stream_event","session_id":"%s","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"working"}}}\n' "$sid"
        printf '{"type":"control_request","request_id":"claude-req-1","session_id":"%s","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","decision_reason":"writes files","input":{"command":"ls -la","timeout_ms":1000}}}\n' "$sid"
        ;;
      *'"request_id":"claude-req-1"'*)
        printf '{"type":"control_request","request_id":"claude-req-2","session_id":"%s","request":{"subtype":"request_user_dialog","header":"Intent","message":"Run the full suite?","options":[{"label":"Yes","description":"Run all checks now"},{"label":"No","description":"Skip the full suite"}]}}\n' "$sid"
        ;;
      *'"request_id":"claude-req-2"'*)
        printf '{"type":"assistant","session_id":"%s","parent_tool_use_id":null,"message":{"role":"assistant","content":[{"type":"text","text":"ignored"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls -la","timeout_ms":1000}}]}}\n' "$sid"
        printf '{"type":"assistant","session_id":"%s","parent_tool_use_id":"toolu_1","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_nested","name":"Grep","input":{}}]}}\n' "$sid"
        printf '{"type":"user","session_id":"%s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false,"content":[{"type":"text","text":"ok"}]}]}}\n' "$sid"
        printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"done","usage":{"input_tokens":11,"cache_read_input_tokens":4,"output_tokens":7}}\n' "$sid"
        ;;
    esac
  done
  exit 0
fi
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"assistant","message":{"type":"message","role":"assistant","content":[{"type":"text","text":"R07_CLAUDE_HANDSHAKE_OK"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"R07_CLAUDE_HANDSHAKE_OK","session_id":"private-session-canary"}'
"##
    }
}

#[tokio::test]
async fn live_installed_claude_probe_is_explicitly_gated() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    use heycode_runtime::{AccountStatus, AgentRuntime as _};
    use heycode_runtime_claude::{ClaudeRuntime, ClaudeRuntimeConfig};
    use tokio_util::sync::CancellationToken;

    let config = ClaudeRuntimeConfig::new(std::env::current_dir().unwrap()).unwrap();
    let runtime = ClaudeRuntime::new(heycode_exec::SubprocessService::local(), config).unwrap();
    let account = runtime.account(CancellationToken::new()).await.unwrap();
    assert_eq!(account.status(), AccountStatus::Connected);
    let receipt = runtime.handshake(CancellationToken::new()).await.unwrap();
    assert!(receipt.no_session_persistence());
    let models = runtime.models(CancellationToken::new()).await.unwrap();
    assert!(!models.models.is_empty());
    let configurations = runtime
        .model_configurations(CancellationToken::new())
        .await
        .unwrap();
    assert!(!configurations.is_empty());
    assert!(
        configurations
            .iter()
            .any(|configuration| !configuration.reasoning_efforts.is_empty())
    );
    assert!(configurations.iter().all(|configuration| {
        models
            .models
            .iter()
            .any(|model| model.id == configuration.model)
    }));
    runtime.close(CancellationToken::new()).await.unwrap();
}

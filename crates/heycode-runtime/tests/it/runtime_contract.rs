//! R01 runtime metadata, lifecycle, raw event and registry contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use heycode_llm::{CapabilitySupport, CatalogSnapshot, ProviderDescriptor};
use heycode_runtime::{
    AccountState, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeKind, AgentRuntimeRegistry,
    RuntimeCapabilities, RuntimeCompactOutcome, RuntimeError, RuntimeErrorCode, RuntimeEvent,
    RuntimeEventKind, RuntimeEventStream, RuntimeFinishReason, RuntimeFork, RuntimeInput,
    RuntimePermissionDecision, RuntimePermissionResponse, RuntimeQuestionResponse, RuntimeResume,
    RuntimeSession, RuntimeSessionId, RuntimeStart, RuntimeTurnId, runtime_registry_plugin,
};
use tokio_util::sync::CancellationToken;

#[test]
fn runtime_configuration_validates_and_redacts_model_visible_controls() {
    let configuration = heycode_runtime::RuntimeConfiguration::new()
        .with_system_prompt("private-prompt-canary")
        .unwrap()
        .with_tools(vec![heycode_core::ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        }])
        .unwrap()
        .with_model("private-model-canary")
        .unwrap()
        .with_reasoning_effort("private-effort-canary")
        .unwrap();
    assert_eq!(configuration.tools()[0].name, "read_file");
    assert!(configuration.tools_configured());
    assert_eq!(
        configuration.reasoning_effort(),
        Some("private-effort-canary")
    );
    let debug = format!("{configuration:?}");
    for secret in [
        "private-prompt-canary",
        "private-model-canary",
        "private-effort-canary",
    ] {
        assert!(!debug.contains(secret));
    }
    assert!(
        heycode_runtime::RuntimeConfiguration::new()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "invalid tool".to_owned(),
                description: "bad".to_owned(),
                parameters: serde_json::json!({"type":"object"}),
            }])
            .is_err()
    );

    let disabled = configuration.merged_with(
        &heycode_runtime::RuntimeConfiguration::new()
            .with_tools(vec![])
            .unwrap(),
    );
    assert!(disabled.tools_configured());
    assert!(disabled.tools().is_empty());
}

#[tokio::test]
async fn compatibility_session_names_each_unsupported_configuration_field() {
    let session = FakeRuntime::new("compat", AgentRuntimeKind::Delegated)
        .session(RuntimeSessionId::new("compat-session").unwrap());
    let update = heycode_runtime::RuntimeConfiguration::new()
        .with_system_prompt("instructions")
        .unwrap()
        .with_model("model")
        .unwrap()
        .with_reasoning_effort("high")
        .unwrap();
    let error = session
        .configure(update, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), RuntimeErrorCode::Unsupported);
    assert!(error.message().contains("system_prompt"));
    assert!(error.message().contains("model"));
    assert!(error.message().contains("reasoning_effort"));

    let unclassified = RuntimeError::unsupported_field_names(&["model", "hostile\nfield"]);
    assert_eq!(unclassified.code(), RuntimeErrorCode::Unsupported);
    assert_eq!(
        unclassified.message(),
        "runtime configuration update is unsupported"
    );
}

fn capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        models: CapabilitySupport::Supported,
        resume: CapabilitySupport::Supported,
        fork: CapabilitySupport::Supported,
        steer: CapabilitySupport::Supported,
        follow_up: CapabilitySupport::Supported,
        permissions: CapabilitySupport::Supported,
        questions: CapabilitySupport::Supported,
        compaction: CapabilitySupport::Supported,
    }
}

struct FakeRuntime {
    descriptor: AgentRuntimeDescriptor,
    actions: Arc<Mutex<Vec<String>>>,
}

impl FakeRuntime {
    fn new(id: &str, kind: AgentRuntimeKind) -> Self {
        Self {
            descriptor: AgentRuntimeDescriptor::new(id, id.to_uppercase(), kind, capabilities())
                .unwrap(),
            actions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn session(&self, id: RuntimeSessionId) -> Arc<dyn RuntimeSession> {
        Arc::new(FakeSession {
            id,
            runtime: self.descriptor.id().clone(),
            capabilities: self.descriptor.capabilities().clone(),
            actions: self.actions.clone(),
            closed: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl AgentRuntime for FakeRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        AccountState::connected(Some("test account"))
            .map_err(|_| RuntimeError::internal("invalid test account"))
    }

    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CatalogSnapshot, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Ok(CatalogSnapshot {
            provider: ProviderDescriptor {
                id: self.descriptor.id().as_str().to_owned(),
                display_name: self.descriptor.display_name().to_owned(),
                protocols: Vec::new(),
            },
            models: Vec::new(),
            revision: 1,
            fetched_at_ms: 1,
        })
    }

    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.actions
            .lock()
            .unwrap()
            .push(format!("start:{}", request.session_id()));
        Ok(self.session(RuntimeSessionId::new("external-1").unwrap()))
    }

    async fn resume(
        &self,
        request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Ok(self.session(request.runtime_session_id().clone()))
    }

    async fn fork(
        &self,
        request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.actions.lock().unwrap().push(format!(
            "fork:{}",
            request.source_runtime_session_id().as_str()
        ));
        Ok(self.session(RuntimeSessionId::new("external-fork").unwrap()))
    }
}

struct FakeSession {
    id: RuntimeSessionId,
    runtime: heycode_runtime::AgentRuntimeId,
    capabilities: RuntimeCapabilities,
    actions: Arc<Mutex<Vec<String>>>,
    closed: AtomicBool,
}

impl FakeSession {
    fn check(&self, cancellation: &CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else if self.closed.load(Ordering::SeqCst) {
            Err(RuntimeError::closed())
        } else {
            Ok(())
        }
    }

    fn action(&self, action: impl Into<String>) {
        self.actions.lock().unwrap().push(action.into());
    }
}

#[async_trait]
impl RuntimeSession for FakeSession {
    fn id(&self) -> &RuntimeSessionId {
        &self.id
    }

    fn runtime_id(&self) -> &heycode_runtime::AgentRuntimeId {
        &self.runtime
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    fn subscribe(&self) -> RuntimeEventStream {
        Box::pin(stream::iter([
            Ok(RuntimeEvent::new(0, RuntimeEventKind::SessionReady)),
            Ok(RuntimeEvent::new(
                1,
                RuntimeEventKind::TurnFinished {
                    turn: RuntimeTurnId::new("turn-1").unwrap(),
                    reason: RuntimeFinishReason::Stop,
                },
            )),
        ]))
    }

    async fn send(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        self.check(&cancellation)?;
        self.action(format!("send:{}", input.text()));
        RuntimeTurnId::new("turn-1").map_err(|_| RuntimeError::internal("invalid turn id"))
    }

    async fn steer(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        self.action(format!("steer:{}", input.text()));
        Ok(())
    }

    async fn follow_up(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        self.action(format!("follow:{}", input.text()));
        Ok(())
    }

    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        self.action("cancel");
        Ok(())
    }

    async fn respond_permission(
        &self,
        response: RuntimePermissionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        self.action(format!("permission:{:?}", response.decision()));
        Ok(())
    }

    async fn respond_question(
        &self,
        response: RuntimeQuestionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        self.check(&cancellation)?;
        self.action(format!(
            "question:{}",
            response.answer().unwrap_or("<cancelled>")
        ));
        Ok(())
    }

    async fn compact(
        &self,
        cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, RuntimeError> {
        self.check(&cancellation)?;
        self.action("compact");
        Ok(RuntimeCompactOutcome::Applied)
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.closed.store(true, Ordering::SeqCst);
        self.action("close");
        Ok(())
    }
}

#[test]
fn metadata_and_boundary_types_reject_ambiguous_values() {
    assert!(
        AgentRuntimeDescriptor::new(
            "Bad_Runtime",
            "Bad",
            AgentRuntimeKind::Delegated,
            RuntimeCapabilities::unknown(),
        )
        .is_err()
    );
    assert!(
        AgentRuntimeDescriptor::new(
            "good-runtime",
            " bad ",
            AgentRuntimeKind::Delegated,
            RuntimeCapabilities::unknown(),
        )
        .is_err()
    );
    assert!(RuntimeSessionId::new("").is_err());
    assert!(RuntimeInput::new("   ").is_err());
    let attachment = heycode_core::AttachmentMetadata::new(
        heycode_core::AttachmentContentId::from_sha256([0x66; 32]),
        heycode_core::AttachmentMediaType::new("image/png").unwrap(),
        10,
        Some("private.png".to_owned()),
        Some(heycode_core::AttachmentDimensions::new(1, 1).unwrap()),
    )
    .unwrap();
    let media = RuntimeInput::with_attachments("", vec![attachment.clone()]).unwrap();
    assert_eq!(media.attachments(), std::slice::from_ref(&attachment));
    assert!(!format!("{media:?}").contains("private.png"));
    assert!(RuntimeInput::with_attachments("media", vec![attachment.clone(), attachment]).is_err());
    assert!(
        RuntimeStart::new(
            heycode_core::SessionId::from_raw("session"),
            "relative/workspace",
        )
        .is_err()
    );
    assert!(RuntimeError::try_new(RuntimeErrorCode::Protocol, "bad\nbody").is_err());
    assert!(AccountState::connected(Some("bad\nlabel")).is_err());

    let descriptor = AgentRuntimeDescriptor::new(
        "codex",
        "Codex subscription",
        AgentRuntimeKind::Delegated,
        RuntimeCapabilities::unknown(),
    )
    .unwrap();
    assert_eq!(descriptor.id().as_str(), "codex");
    assert_eq!(descriptor.kind(), AgentRuntimeKind::Delegated);
    assert_eq!(descriptor.capabilities().resume, CapabilitySupport::Unknown);
}

#[tokio::test]
async fn session_contract_covers_discovery_start_resume_fork_controls_events_and_close() {
    let runtime = FakeRuntime::new("native", AgentRuntimeKind::Native);
    let cancellation = CancellationToken::new();
    assert_eq!(
        runtime
            .account(cancellation.clone())
            .await
            .unwrap()
            .status(),
        heycode_runtime::AccountStatus::Connected
    );
    assert_eq!(
        runtime
            .models(cancellation.clone())
            .await
            .unwrap()
            .provider
            .id,
        "native"
    );
    let workspace = std::env::current_dir().unwrap();
    let start = RuntimeStart::new(heycode_core::SessionId::from_raw("local-1"), &workspace)
        .unwrap()
        .with_model("model-1")
        .unwrap();
    let session = runtime.start(start, cancellation.clone()).await.unwrap();
    assert_eq!(session.id().as_str(), "external-1");
    assert_eq!(session.runtime_id().as_str(), "native");

    let mut events = session.subscribe();
    assert!(matches!(
        events.next().await.unwrap().unwrap().kind(),
        RuntimeEventKind::SessionReady
    ));
    assert_eq!(events.next().await.unwrap().unwrap().sequence(), 1);
    assert_eq!(
        session
            .send(RuntimeInput::new("build it").unwrap(), cancellation.clone())
            .await
            .unwrap()
            .as_str(),
        "turn-1"
    );
    session
        .steer(
            RuntimeInput::new("focus tests").unwrap(),
            cancellation.clone(),
        )
        .await
        .unwrap();
    session
        .follow_up(
            RuntimeInput::new("then docs").unwrap(),
            cancellation.clone(),
        )
        .await
        .unwrap();
    session
        .respond_permission(
            RuntimePermissionResponse::new(
                heycode_runtime::RuntimeRequestId::new("permission-1").unwrap(),
                RuntimePermissionDecision::AllowOnce,
            ),
            cancellation.clone(),
        )
        .await
        .unwrap();
    session
        .respond_question(
            RuntimeQuestionResponse::new(
                heycode_runtime::RuntimeRequestId::new("question-1").unwrap(),
                "yes",
            )
            .unwrap(),
            cancellation.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        session.compact(cancellation.clone()).await.unwrap(),
        RuntimeCompactOutcome::Applied
    );

    let resume = RuntimeResume::new(
        heycode_core::SessionId::from_raw("local-2"),
        &workspace,
        RuntimeSessionId::new("external-resume").unwrap(),
    )
    .unwrap();
    assert_eq!(
        runtime
            .resume(resume, cancellation.clone())
            .await
            .unwrap()
            .id()
            .as_str(),
        "external-resume"
    );
    let fork = RuntimeFork::new(
        heycode_core::SessionId::from_raw("local-3"),
        &workspace,
        RuntimeSessionId::new("external-1").unwrap(),
    )
    .unwrap();
    assert_eq!(
        runtime
            .fork(fork, cancellation.clone())
            .await
            .unwrap()
            .id()
            .as_str(),
        "external-fork"
    );

    session.cancel(cancellation.clone()).await.unwrap();
    session.close(cancellation.clone()).await.unwrap();
    let error = session
        .send(RuntimeInput::new("too late").unwrap(), cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.code(), RuntimeErrorCode::Closed);
}

#[tokio::test]
async fn cancellation_is_explicit_and_does_not_become_an_internal_failure() {
    let runtime = FakeRuntime::new("claude", AgentRuntimeKind::Delegated);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = runtime.account(cancellation).await.unwrap_err();
    assert_eq!(error.code(), RuntimeErrorCode::Cancelled);
    assert_eq!(error.to_string(), "runtime operation cancelled");
}

#[test]
fn registry_is_sorted_unique_and_effect_owned_and_plugin_publishes_service() {
    let plugins = vec![runtime_registry_plugin()];
    let mut composed = heycode_core::compose(&plugins).unwrap();
    let published = composed
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    assert!(published.is_empty().unwrap());
    assert_eq!(
        composed.owner_of(heycode_runtime::SERVICE_RUNTIMES),
        Some("runtimes")
    );

    published
        .register(
            &composed,
            Arc::new(FakeRuntime::new("zeta", AgentRuntimeKind::Delegated)),
        )
        .unwrap();
    published
        .register(
            &composed,
            Arc::new(FakeRuntime::new("alpha", AgentRuntimeKind::Native)),
        )
        .unwrap();
    assert_eq!(
        published.ids().unwrap(),
        ["alpha".to_owned(), "zeta".to_owned()]
    );
    assert!(published.get("alpha").unwrap().is_some());
    assert!(
        published
            .register(
                &composed,
                Arc::new(FakeRuntime::new("alpha", AgentRuntimeKind::Native)),
            )
            .is_err()
    );

    composed.shutdown();
    assert!(published.is_empty().unwrap());
}

#[test]
fn runtime_inventory_kind_is_distinct_from_inference_provider() {
    assert_eq!(
        heycode_core::ContributionKind::AgentRuntime.as_str(),
        "agent_runtime"
    );
    assert_ne!(
        heycode_core::ContributionKind::AgentRuntime,
        heycode_core::ContributionKind::InferenceProvider
    );

    struct RuntimeClaim;
    impl heycode_core::Plugin for RuntimeClaim {
        fn name(&self) -> &'static str {
            "runtime-claim"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                "1",
                &[heycode_core::PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::AgentRuntime,
                "fake-runtime",
            )]
        }

        fn apply(&self, _context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            Ok(())
        }
    }
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![Box::new(RuntimeClaim)];
    let context = heycode_core::compose(&plugins).unwrap();
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(
                |row| row.kind == heycode_core::ContributionKind::AgentRuntime
                    && row.name == "fake-runtime"
            )
    );
}

#[test]
fn primary_admission_does_not_require_optional_controls_or_upgrade_unknown_permissions() {
    let mut capabilities = heycode_runtime::RuntimeCapabilities::unknown();
    assert!(!capabilities.supports_primary_sessions());
    capabilities.permissions = heycode_llm::CapabilitySupport::Supported;
    assert!(capabilities.supports_primary_sessions());
    assert_eq!(capabilities.fork, heycode_llm::CapabilitySupport::Unknown);
    capabilities.permissions = heycode_llm::CapabilitySupport::Unsupported;
    assert!(!capabilities.supports_primary_sessions());
}

#[test]
fn runtime_connection_help_is_bounded_and_survives_descriptor_cloning() {
    let descriptor = AgentRuntimeDescriptor::new(
        "test",
        "Test",
        AgentRuntimeKind::Delegated,
        RuntimeCapabilities::unknown(),
    )
    .unwrap();
    assert!(
        descriptor
            .clone()
            .with_connection_help("bad\nhelp")
            .is_err()
    );
    assert!(
        descriptor
            .clone()
            .with_connection_help("x".repeat(513))
            .is_err()
    );
    let descriptor = descriptor
        .with_connection_help("Install the official app and run its login command.")
        .unwrap();
    assert_eq!(
        descriptor.clone().connection_help(),
        Some("Install the official app and run its login command.")
    );
}

#[test]
fn question_response_preserves_selected_labels_separately_from_custom_text_and_cancel() {
    use heycode_runtime::{RuntimeQuestionResponse, RuntimeRequestId};
    let request = || RuntimeRequestId::new("question-array").unwrap();
    let selected =
        RuntimeQuestionResponse::selected(request(), vec!["Unit".into(), "Integration".into()])
            .unwrap();
    assert_eq!(
        selected.selected_answers().unwrap(),
        &["Unit", "Integration"]
    );
    assert!(selected.answer().is_none());
    let custom = RuntimeQuestionResponse::new(request(), "[\"Unit\",\"Integration\"]").unwrap();
    assert!(custom.selected_answers().is_none());
    assert_eq!(custom.answer(), Some("[\"Unit\",\"Integration\"]"));
    let cancelled = RuntimeQuestionResponse::cancelled(request());
    assert!(cancelled.answer().is_none() && cancelled.selected_answers().is_none());
    for invalid in [
        vec![],
        vec!["Unit".into(), "Unit".into()],
        vec![" ".into()],
        vec!["bad\0label".into()],
    ] {
        assert!(RuntimeQuestionResponse::selected(request(), invalid).is_err());
    }
}

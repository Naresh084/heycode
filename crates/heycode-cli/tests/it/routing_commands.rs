//! CMD02 real composition, durable selection, command ownership and connect flow.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_cli::testing::RealCompositionHarness;

struct EffortAwareProvider;

struct FailingDeleteCredentialProvider {
    id: heycode_credentials::CredentialProviderId,
    reference: String,
}

impl heycode_credentials::CredentialProvider for FailingDeleteCredentialProvider {
    fn id(&self) -> &heycode_credentials::CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(
        &self,
        query: &heycode_credentials::CredentialQuery,
    ) -> Result<heycode_credentials::CredentialProviderState, String> {
        Ok(if query.reference.as_str() == self.reference {
            heycode_credentials::CredentialProviderState::configured(
                heycode_credentials::CredentialSource::File,
                true,
            )
        } else {
            heycode_credentials::CredentialProviderState::unconfigured(false)
        })
    }

    fn resolve(
        &self,
        query: &heycode_credentials::CredentialQuery,
    ) -> Result<Option<heycode_credentials::CredentialSecret>, String> {
        Ok((query.reference.as_str() == self.reference)
            .then(|| heycode_credentials::CredentialSecret::new("failing-delete-secret")))
    }

    fn delete(&self, _query: &heycode_credentials::CredentialQuery) -> Result<(), String> {
        Err("fixture delete failed".to_owned())
    }
}

type ConfigureHook = Box<dyn FnOnce() + Send>;

struct FakeDelegatedControls {
    runtime: String,
    catalog: heycode_llm::CatalogSnapshot,
    model_configurations: Vec<heycode_runtime::RuntimeModelConfiguration>,
    configuration: Mutex<heycode_runtime::RuntimeConfiguration>,
    configure_calls: AtomicUsize,
    invalidate_calls: AtomicUsize,
    disconnect_calls: AtomicUsize,
    fail_next: AtomicBool,
    usable: AtomicBool,
    after_configure: Mutex<Option<ConfigureHook>>,
}

impl FakeDelegatedControls {
    fn codex() -> Self {
        let configuration = heycode_runtime::RuntimeConfiguration::new()
            .with_system_prompt("retained delegated instructions")
            .unwrap()
            .with_tools(vec![heycode_core::ToolSpec {
                name: "retained_tool".to_owned(),
                description: "A retained tool definition".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }])
            .unwrap()
            .with_model("codex-old")
            .unwrap()
            .with_reasoning_effort("low")
            .unwrap();
        Self {
            runtime: "codex".to_owned(),
            catalog: heycode_llm::CatalogSnapshot {
                provider: heycode_llm::ProviderDescriptor {
                    id: "codex".to_owned(),
                    display_name: "Codex fixture".to_owned(),
                    protocols: vec![heycode_llm::ProviderProtocol::DelegatedAgent],
                },
                models: vec![
                    heycode_llm::ModelDescriptor::unknown("codex-old"),
                    heycode_llm::ModelDescriptor::unknown("codex-new"),
                ],
                revision: 7,
                fetched_at_ms: 1,
            },
            model_configurations: vec![heycode_runtime::RuntimeModelConfiguration {
                model: "codex-new".to_owned(),
                display_name: "Codex New".to_owned(),
                resolved_model: None,
                description: None,
                context_window: None,
                default_reasoning_effort: Some("medium".to_owned()),
                reasoning_efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
            }],
            configuration: Mutex::new(configuration),
            configure_calls: AtomicUsize::new(0),
            invalidate_calls: AtomicUsize::new(0),
            disconnect_calls: AtomicUsize::new(0),
            fail_next: AtomicBool::new(false),
            usable: AtomicBool::new(true),
            after_configure: Mutex::new(None),
        }
    }

    fn configuration(&self) -> heycode_runtime::RuntimeConfiguration {
        self.configuration.lock().unwrap().clone()
    }

    fn after_next_configure(&self, hook: impl FnOnce() + Send + 'static) {
        *self.after_configure.lock().unwrap() = Some(Box::new(hook));
    }
}

#[async_trait::async_trait]
impl heycode_routing::DelegatedRuntimeControls for FakeDelegatedControls {
    fn active_runtime_id(&self) -> Result<String, String> {
        if !self.usable.load(Ordering::SeqCst) {
            return Err("delegated backend session was retired".to_owned());
        }
        Ok(self.runtime.clone())
    }

    async fn models(
        &self,
        expected_runtime: &str,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, String> {
        if expected_runtime != self.runtime {
            return Err("unexpected runtime".to_owned());
        }
        Ok(self.catalog.clone())
    }

    async fn model_configurations(
        &self,
        expected_runtime: &str,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Vec<heycode_runtime::RuntimeModelConfiguration>, String> {
        if expected_runtime != self.runtime {
            return Err("unexpected runtime".to_owned());
        }
        Ok(self.model_configurations.clone())
    }

    async fn configure(
        &self,
        expected_runtime: &str,
        update: heycode_runtime::RuntimeConfiguration,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_routing::AppliedDelegatedConfiguration, String> {
        if expected_runtime != self.runtime {
            return Err("unexpected runtime".to_owned());
        }
        if !self.usable.load(Ordering::SeqCst) {
            return Err("delegated backend session was retired".to_owned());
        }
        self.configure_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err("fixture configuration failure".to_owned());
        }
        let mut configuration = self.configuration.lock().unwrap();
        *configuration = configuration.merged_with(&update);
        let effective = configuration.clone();
        drop(configuration);
        if let Some(hook) = self.after_configure.lock().unwrap().take() {
            hook();
        }
        Ok(heycode_routing::AppliedDelegatedConfiguration::new(
            effective, 1,
        ))
    }

    async fn invalidate(
        &self,
        expected_runtime: &str,
        backend_generation: u64,
    ) -> Result<(), String> {
        if expected_runtime == self.runtime && backend_generation == 1 {
            self.invalidate_calls.fetch_add(1, Ordering::SeqCst);
            self.usable.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn disconnect(&self, expected_runtime: &str) -> Result<(), String> {
        if expected_runtime != self.runtime {
            return Err("unexpected runtime".to_owned());
        }
        self.disconnect_calls.fetch_add(1, Ordering::SeqCst);
        self.usable.store(false, Ordering::SeqCst);
        Ok(())
    }
}

fn effort_provider_descriptor() -> heycode_llm::ProviderDescriptor {
    heycode_llm::ProviderDescriptor {
        id: "fake".to_owned(),
        display_name: "Effort-aware fake".to_owned(),
        protocols: vec![heycode_llm::ProviderProtocol::OpenAiChatCompletions],
    }
}

#[async_trait::async_trait]
impl heycode_llm::Provider for EffortAwareProvider {
    fn info(&self) -> heycode_llm::ProviderInfo {
        heycode_llm::ProviderInfo {
            name: "fake".to_owned(),
            default_model: "fake-model".to_owned(),
        }
    }

    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        effort_provider_descriptor()
    }

    fn describe_model(&self, model: &str) -> heycode_llm::ModelDescriptor {
        let mut descriptor = heycode_llm::ModelDescriptor::unknown(model);
        if matches!(model, "fake-model" | "fake-other") {
            descriptor.capabilities.reasoning = heycode_llm::CapabilitySupport::Supported;
        }
        descriptor
    }

    fn inference_adapter(&self) -> Option<&dyn heycode_llm::InferenceAdapter> {
        Some(self)
    }

    fn stream(&self, _request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
        Box::pin(futures::stream::empty())
    }
}

impl heycode_llm::InferenceAdapter for EffortAwareProvider {
    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        effort_provider_descriptor()
    }

    fn authentication_binding(&self) -> heycode_llm::AuthenticationBinding {
        heycode_llm::AuthenticationBinding::None
    }

    fn reasoning_effort_options(
        &self,
        model: &heycode_llm::ModelDescriptor,
    ) -> Result<Option<heycode_llm::ReasoningEffortOptions>, heycode_llm::ResolveError> {
        heycode_llm::ReasoningEffortOptions::for_model(
            model,
            vec![
                heycode_llm::ReasoningEffortId::new("low")?,
                heycode_llm::ReasoningEffortId::new("high")?,
            ],
            Some(heycode_llm::ReasoningEffortId::new("low")?),
        )
    }

    fn resolve(
        &self,
        _draft: heycode_llm::RequestDraft,
        _model: &heycode_llm::ModelDescriptor,
    ) -> Result<heycode_llm::ResolvedCall, heycode_llm::ResolveError> {
        Err(heycode_llm::ResolveError::InvalidAdapter {
            field: "test",
            message: "test provider does not dispatch".to_owned(),
        })
    }

    fn stream(&self, _call: heycode_llm::ResolvedCall) -> heycode_llm::InferenceStream {
        Box::pin(futures::stream::empty())
    }
}

fn session_directory_count(root: &std::path::Path) -> usize {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .count()
}

#[tokio::test]
async fn native_effort_command_uses_exact_adapter_metadata_and_persists_before_live_apply() {
    let harness = RealCompositionHarness::new().unwrap();
    let model = heycode_llm::Provider::describe_model(&EffortAwareProvider, "fake-model");
    harness
        .seed_catalog_snapshot(heycode_llm::CatalogSnapshot {
            provider: effort_provider_descriptor(),
            models: vec![model],
            revision: 1,
            fetched_at_ms: 1,
        })
        .unwrap();
    let harness = harness.with_provider(Arc::new(EffortAwareProvider));
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let context = world.context();
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    routing.select_provider("fake").unwrap();
    let effort = commands.get("effort").unwrap().unwrap();
    assert!(
        effort.availability().is_available(),
        "{:?}",
        routing.effort_options()
    );
    let picker_configuration = routing.active_configuration().unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        sink.lock().unwrap().push(event.clone());
    });
    effort.execute(&agent, "").await.unwrap();
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::EffortPickerRequested {
            owner: heycode_agent::BackendControlOwner::NativeInference { provider },
            routing_revision,
            current_effort: None,
            choices,
            default_effort: Some(default),
        } if provider == "fake"
            && *routing_revision == picker_configuration.revision()
            && choices == &["low", "high"]
            && default == "low"
    )));

    effort.execute(&agent, "high").await.unwrap();
    assert_eq!(routing.selection().unwrap().effort(), Some("high"));
    assert_eq!(
        agent
            .reasoning_effort()
            .as_ref()
            .map(|value| value.as_str()),
        Some("high")
    );
    assert!(
        std::fs::read_to_string(&settings_path)
            .unwrap()
            .contains("effort = \"high\"")
    );
    assert!(
        routing
            .select_effort_owned(
                picker_configuration.owner(),
                picker_configuration.revision(),
                "low",
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .is_err(),
        "a picker from the prior routing revision must be stale"
    );
    assert_eq!(routing.selection().unwrap().effort(), Some("high"));
    assert!(effort.execute(&agent, "ultra").await.is_err());
    assert_eq!(routing.selection().unwrap().effort(), Some("high"));
    assert_eq!(
        agent
            .reasoning_effort()
            .as_ref()
            .map(|value| value.as_str()),
        Some("high")
    );
    let persisted = std::fs::read(&settings_path).unwrap();
    let active = routing.active_configuration().unwrap();
    routing
        .select_effort_owned_in_scope(
            active.owner(),
            active.revision(),
            "low",
            heycode_routing::SelectionScope::Session,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(routing.selection().unwrap().effort(), Some("low"));
    assert_eq!(
        agent
            .reasoning_effort()
            .as_ref()
            .map(|value| value.as_str()),
        Some("low")
    );
    assert_eq!(std::fs::read(&settings_path).unwrap(), persisted);
    let session_active = routing.active_configuration().unwrap();
    let choices = routing
        .effort_catalog_owned(
            session_active.owner(),
            session_active.revision(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(choices.current(), Some("low"));
    assert_eq!(choices.default(), Some("high"));
    assert!(
        routing
            .select_effort_owned_in_scope(
                active.owner(),
                active.revision(),
                "high",
                heycode_routing::SelectionScope::Session,
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    world.shutdown();
}

#[tokio::test]
async fn delegated_model_and_effort_apply_live_then_persist_as_one_independent_tuple() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let native = routing.selection().unwrap();
    let mut fixture_controls = FakeDelegatedControls::codex();
    let mut alternate = fixture_controls.model_configurations[0].clone();
    alternate.model = "codex-old".to_owned();
    alternate.display_name = "Codex Old".to_owned();
    fixture_controls.model_configurations.push(alternate);
    let controls = Arc::new(fixture_controls);
    routing
        .register_delegated_controls(controls.clone())
        .unwrap();
    routing.select_runtime("codex").unwrap();

    let active = routing.active_configuration().unwrap();
    assert_eq!(
        active.owner(),
        &heycode_agent::BackendControlOwner::DelegatedRuntime {
            runtime: "codex".to_owned(),
        }
    );
    let catalog = routing
        .models_owned(
            active.owner(),
            heycode_llm::CatalogRefreshMode::Force,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let selected = routing
        .select_model_owned(
            active.owner(),
            active.revision(),
            Some(catalog.snapshot.as_ref()),
            "codex-new",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(selected.runtime_model(), Some("codex-new"));
    assert_eq!(selected.runtime_effort(), Some("low"));

    let active = routing.active_configuration().unwrap();
    let model_configuration = routing
        .model_configuration_owned(
            active.owner(),
            active.revision(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(model_configuration.model, "codex-new");
    assert_eq!(model_configuration.display_name, "Codex New");
    assert_eq!(
        model_configuration.default_reasoning_effort.as_deref(),
        Some("medium")
    );
    let effort_catalog = routing
        .effort_catalog_owned(
            active.owner(),
            active.revision(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(effort_catalog.current(), Some("low"));
    assert_eq!(effort_catalog.choices(), ["low", "medium", "high"]);
    assert_eq!(effort_catalog.default(), Some("low"));
    let selected = routing
        .select_effort_owned(
            active.owner(),
            active.revision(),
            "high",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(selected.runtime(), "codex");
    assert_eq!(selected.runtime_model(), Some("codex-new"));
    assert_eq!(selected.runtime_effort(), Some("high"));
    assert!(
        routing
            .model_configuration_owned(
                active.owner(),
                active.revision(),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .is_err(),
        "metadata from the prior routing revision must be stale"
    );
    assert_eq!(selected.provider(), native.provider());
    assert_eq!(selected.model(), native.model());
    assert_eq!(selected.effort(), native.effort());
    let configured = controls.configuration();
    assert_eq!(
        configured.system_prompt(),
        Some("retained delegated instructions")
    );
    assert_eq!(configured.tools().len(), 1);
    assert_eq!(configured.tools()[0].name, "retained_tool");
    assert_eq!(configured.model(), Some("codex-new"));
    assert_eq!(configured.reasoning_effort(), Some("high"));
    assert_eq!(controls.configure_calls.load(Ordering::SeqCst), 2);
    let persisted = std::fs::read_to_string(&settings_path).unwrap();
    assert!(
        persisted.contains("runtime_model = \"codex-new\""),
        "{persisted}"
    );
    assert!(
        persisted.contains("runtime_effort = \"high\""),
        "{persisted}"
    );
    assert!(
        persisted.contains(&format!("provider = \"{}\"", native.provider())),
        "{persisted}"
    );
    assert!(
        persisted.contains(&format!("model = \"{}\"", native.model())),
        "{persisted}"
    );
    let active = routing.active_configuration().unwrap();
    routing
        .select_effort_owned_in_scope(
            active.owner(),
            active.revision(),
            "medium",
            heycode_routing::SelectionScope::Session,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        routing.selection().unwrap().runtime_effort(),
        Some("medium")
    );
    assert_eq!(controls.configuration().reasoning_effort(), Some("medium"));
    assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), persisted);
    let active = routing.active_configuration().unwrap();
    routing
        .select_model_owned_in_scope(
            active.owner(),
            active.revision(),
            Some(catalog.snapshot.as_ref()),
            "codex-old",
            heycode_routing::SelectionScope::Session,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        routing.selection().unwrap().runtime_model(),
        Some("codex-old")
    );
    assert_eq!(controls.configuration().model(), Some("codex-old"));
    assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), persisted);
    let active = routing.active_configuration().unwrap();
    routing
        .select_model_owned_in_scope(
            active.owner(),
            active.revision(),
            Some(catalog.snapshot.as_ref()),
            "codex-new",
            heycode_routing::SelectionScope::Session,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), persisted);
    let active = routing.active_configuration().unwrap();
    let calls_before = controls.configure_calls.load(Ordering::SeqCst);
    assert!(
        routing
            .select_model_configuration_owned(
                active.owner(),
                active.revision(),
                Some(catalog.snapshot.as_ref()),
                heycode_routing::ModelControlChoice {
                    model: "codex-old",
                    effort: Some("unsupported"),
                    scope: heycode_routing::SelectionScope::Session
                },
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        controls.configure_calls.load(Ordering::SeqCst),
        calls_before
    );
    let combined = routing
        .select_model_configuration_owned(
            active.owner(),
            active.revision(),
            Some(catalog.snapshot.as_ref()),
            heycode_routing::ModelControlChoice {
                model: "codex-old",
                effort: Some("high"),
                scope: heycode_routing::SelectionScope::Session,
            },
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(combined.runtime_model(), Some("codex-old"));
    assert_eq!(combined.runtime_effort(), Some("high"));
    assert_eq!(controls.configuration().model(), Some("codex-old"));
    assert_eq!(controls.configuration().reasoning_effort(), Some("high"));
    assert_eq!(
        controls.configure_calls.load(Ordering::SeqCst),
        calls_before + 1
    );
    assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), persisted);
    assert!(
        routing
            .select_model_configuration_owned(
                active.owner(),
                active.revision(),
                Some(catalog.snapshot.as_ref()),
                heycode_routing::ModelControlChoice {
                    model: "codex-new",
                    effort: Some("low"),
                    scope: heycode_routing::SelectionScope::Default
                },
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        controls.configure_calls.load(Ordering::SeqCst),
        calls_before + 1
    );
    let active = routing.active_configuration().unwrap();
    routing
        .select_effort_owned(
            active.owner(),
            active.revision(),
            "low",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(controls.configuration().reasoning_effort(), Some("low"));
    assert!(
        std::fs::read_to_string(&settings_path)
            .unwrap()
            .contains("runtime_effort = \"low\"")
    );
    world.shutdown();
}

#[tokio::test]
async fn stale_or_failed_delegated_apply_cannot_publish_routing_state() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let controls = Arc::new(FakeDelegatedControls::codex());
    routing
        .register_delegated_controls(controls.clone())
        .unwrap();
    routing.select_runtime("codex").unwrap();
    let stale = routing.active_configuration().unwrap();
    let catalog = controls.catalog.clone();

    routing.select_runtime("native").unwrap();
    assert!(
        routing
            .select_model_owned(
                stale.owner(),
                stale.revision(),
                Some(&catalog),
                "codex-new",
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .is_err()
    );
    assert_eq!(controls.configure_calls.load(Ordering::SeqCst), 0);

    routing.select_runtime("codex").unwrap();
    let active = routing.active_configuration().unwrap();
    let selection_before = routing.selection().unwrap();
    let settings_before = std::fs::read_to_string(&settings_path).unwrap();
    let backend_before = controls.configuration();
    controls.fail_next.store(true, Ordering::SeqCst);
    assert!(
        routing
            .select_model_owned(
                active.owner(),
                active.revision(),
                Some(&catalog),
                "codex-new",
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .is_err()
    );
    assert_eq!(controls.configure_calls.load(Ordering::SeqCst), 1);
    assert_eq!(controls.configuration(), backend_before);
    assert_eq!(routing.selection().unwrap(), selection_before);
    assert_eq!(
        std::fs::read_to_string(settings_path).unwrap(),
        settings_before
    );
    world.shutdown();
}

#[tokio::test]
async fn settings_race_after_wire_apply_retires_the_exact_delegated_backend() {
    let harness = RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let controls = Arc::new(FakeDelegatedControls::codex());
    routing
        .register_delegated_controls(controls.clone())
        .unwrap();
    routing.select_runtime("codex").unwrap();
    let active = routing.active_configuration().unwrap();
    let selection_before = routing.selection().unwrap();
    let backend_before = controls.configuration();
    let catalog = controls.catalog.clone();

    let namespace = heycode_routing::settings_namespace().unwrap();
    controls.after_next_configure(move || {
        let snapshot = settings.get(&namespace).unwrap().unwrap();
        settings
            .replace_user(
                &namespace,
                snapshot.user().unwrap().clone(),
                Some(snapshot.revision()),
            )
            .unwrap();
    });
    let error = routing
        .select_model_owned(
            active.owner(),
            active.revision(),
            Some(&catalog),
            "codex-new",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        &error,
        heycode_routing::RoutingError::BackendSessionRetired
    ));
    assert!(error.to_string().contains("reopen the session"));
    assert_eq!(routing.selection().unwrap(), selection_before);
    assert_ne!(controls.configuration(), backend_before);
    assert_eq!(controls.configuration().model(), Some("codex-new"));
    assert_eq!(controls.invalidate_calls.load(Ordering::SeqCst), 1);
    assert!(!controls.usable.load(Ordering::SeqCst));
    assert!(
        heycode_routing::DelegatedRuntimeControls::configure(
            controls.as_ref(),
            "codex",
            heycode_runtime::RuntimeConfiguration::new()
                .with_model("codex-old")
                .unwrap(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .is_err(),
        "the wire-mutated backend must not remain usable"
    );
    world.shutdown();
}

#[tokio::test]
async fn connection_catalog_cannot_admit_a_model_without_a_composed_adapter() {
    let harness = RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|row| row.registry_name == "google")
        .unwrap();
    let catalog = heycode_llm::CatalogSnapshot {
        provider: profile.descriptor.clone(),
        models: vec![heycode_llm::ModelDescriptor::unknown("gemini-catalog-only")],
        revision: 1,
        fetched_at_ms: 1,
    };
    assert!(
        routing
            .stage_connection("google", "gemini-catalog-only", Some(&catalog))
            .is_err()
    );
    routing
        .stage_connection("google", profile.default_model.as_deref().unwrap(), None)
        .unwrap();
}

fn opaque_checkpoint() -> heycode_core::ProviderStateItem {
    heycode_core::ProviderStateItem::new(
        "fake",
        "old-model",
        heycode_core::ProviderProtocol::OpenAiChatCompletions,
        heycode_core::ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant",
            "content":"opaque checkpoint",
            "reasoning_content":"retained state"
        }),
    )
    .unwrap()
}

#[tokio::test]
async fn production_routing_commands_persist_before_live_apply_and_connect_reuses_onboarding() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let context = world.context();
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let initial = routing.selection().unwrap();
    assert_eq!(initial.runtime(), "native");

    let selected = routing.select_provider("fake").unwrap();
    assert_eq!(selected.provider(), "fake");
    assert_eq!(selected.model(), "fake-model");
    assert_eq!(agent.selection().provider_name, "fake");
    assert_eq!(agent.selection().model, "fake-model");
    let selected_runtime = routing.select_runtime("codex").unwrap();
    assert_eq!(selected_runtime.runtime(), "codex");
    let persisted = std::fs::read_to_string(&settings_path).unwrap();
    assert!(persisted.contains("[settings.routing]"), "{persisted}");
    assert!(persisted.contains("provider = \"fake\""), "{persisted}");
    assert!(persisted.contains("model = \"fake-model\""), "{persisted}");
    assert!(persisted.contains("runtime = \"codex\""), "{persisted}");

    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(&heycode_routing::settings_namespace().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.revision(), 2);

    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    for id in ["connect", "logout", "provider", "model", "effort"] {
        let command = commands.get(id).unwrap().unwrap();
        assert_eq!(
            command.descriptor().source().plugin(),
            if matches!(id, "connect" | "logout") {
                "routing-auth"
            } else {
                "routing"
            }
        );
    }
    assert!(
        commands
            .get("effort")
            .unwrap()
            .unwrap()
            .availability()
            .is_available(),
        "delegated routes expose the live effort command through app-server controls"
    );

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        sink.lock().unwrap().push(event.clone());
    });
    commands
        .get("provider")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();
    commands
        .get("model")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::RoutePickerRequested {
            current_provider,
            current_runtime,
        } if current_provider == "fake" && current_runtime == "codex"
    )));
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::ModelPickerRequested {
            owner: heycode_agent::BackendControlOwner::DelegatedRuntime { runtime },
            routing_revision: _,
            current_model,
        } if runtime == "codex" && current_model.is_empty()
    )));

    settings
        .replace_user(
            &heycode_routing::settings_namespace().unwrap(),
            serde_json::json!({
                "runtime": "codex",
                "provider": "fake",
                "model": "external-model"
            }),
            Some(2),
        )
        .unwrap();
    assert_eq!(agent.selection().model, "external-model");

    commands
        .get("connect")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();
    let onboarding = context
        .get::<heycode_onboarding::OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
        .unwrap()
        .snapshot()
        .unwrap();
    assert!(onboarding.active);
    assert_eq!(
        onboarding.step,
        heycode_onboarding::OnboardingStep::RuntimeClass
    );

    let logout = commands.get("logout").unwrap().unwrap();
    assert_eq!(
        logout.descriptor().timing(),
        heycode_agent::CommandTiming::Immediate
    );
    logout.execute(&agent, "").await.unwrap();
    assert!(!agent.inference_connected());
    assert!(matches!(
        routing.active_configuration(),
        Err(heycode_routing::RoutingError::SetupRequired)
    ));
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::LoggedOut { target, .. } if target == "codex"
    )));
    let persisted = std::fs::read_to_string(&settings_path).unwrap();
    assert!(persisted.contains("setup_required = true"), "{persisted}");
    assert!(!persisted.contains("provider ="), "{persisted}");
    world.shutdown();
}

#[tokio::test]
async fn native_logout_deletes_the_exact_saved_custom_credential_reference() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "fake"
model = "fake-model"
credential_reference = "HEYCODE_LOGOUT_CUSTOM_KEY"
"#,
    )
    .unwrap();
    heycode_cli::write_credential_at(
        "HEYCODE_LOGOUT_CUSTOM_KEY",
        "logout-test-secret",
        &harness.credentials_root(),
    )
    .unwrap();
    let credentials_root = harness.credentials_root();
    let world = harness.compose().unwrap();
    let commands = world
        .context()
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    commands
        .get("logout")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();

    assert!(
        heycode_cli::lookup_credential_at("HEYCODE_LOGOUT_CUSTOM_KEY", &credentials_root)
            .unwrap()
            .is_none()
    );
    assert!(!agent.inference_connected());
    world.shutdown();
}

#[tokio::test]
async fn credential_cleanup_failure_still_disconnects_active_inference_and_requests_welcome() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.api_key_env = Some("HEYCODE_LOGOUT_FAILING_KEY".to_owned());
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "fake"
model = "fake-model"
credential_reference = "HEYCODE_LOGOUT_FAILING_KEY"
"#,
    )
    .unwrap();
    let world = harness.compose().unwrap();
    let credentials = world
        .context()
        .get::<heycode_credentials::CredentialsService>(heycode_credentials::SERVICE_CREDENTIALS)
        .unwrap();
    credentials
        .register(
            world.context(),
            Arc::new(FailingDeleteCredentialProvider {
                id: heycode_credentials::CredentialProviderId::new("failing-delete").unwrap(),
                reference: "HEYCODE_LOGOUT_FAILING_KEY".to_owned(),
            }),
        )
        .unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        sink.lock().unwrap().push(event.clone());
    });

    world
        .context()
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("logout")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();

    assert!(!agent.inference_connected());
    assert!(agent.send("must remain blocked").await.is_err());
    assert!(matches!(
        routing.active_configuration(),
        Err(heycode_routing::RoutingError::SetupRequired)
    ));
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::LoggedOut {
            cleanup_warning: Some(warning),
            ..
        } if warning.contains("failing-delete")
    )));
    world.shutdown();
}

#[tokio::test]
async fn delegated_default_logout_preserves_the_dormant_native_credential() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.api_key_env = Some("HEYCODE_DORMANT_NATIVE_KEY".to_owned());
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "codex"
provider = "fake"
model = "fake-model"
credential_reference = "HEYCODE_DORMANT_NATIVE_KEY"
"#,
    )
    .unwrap();
    heycode_cli::write_credential_at(
        "HEYCODE_DORMANT_NATIVE_KEY",
        "dormant-native-secret",
        &harness.credentials_root(),
    )
    .unwrap();
    let credentials_root = harness.credentials_root();
    let world = harness.compose().unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        sink.lock().unwrap().push(event.clone());
    });

    world
        .context()
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("logout")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();

    assert!(
        heycode_cli::lookup_credential_at("HEYCODE_DORMANT_NATIVE_KEY", &credentials_root)
            .unwrap()
            .is_some()
    );
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::LoggedOut {
            target,
            cleanup_warning: None,
        } if target == "codex"
    )));
    world.shutdown();
}

#[test]
fn persisted_routing_selection_restores_during_real_composition() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "codex"
provider = "fake"
model = "fake-model"
"#,
    )
    .unwrap();
    let world = harness.compose().unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    assert_eq!(routing.selection().unwrap().runtime(), "codex");
    assert_eq!(agent.selection().provider_name, "fake");
    assert_eq!(agent.selection().model, "fake-model");
    world.shutdown();
}

#[test]
fn persisted_native_effort_restores_into_the_live_agent_during_real_composition() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "fake"
model = "fake-model"
effort = "high"
"#,
    )
    .unwrap();
    let world = harness
        .with_provider(Arc::new(EffortAwareProvider))
        .compose()
        .unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();

    assert_eq!(routing.selection().unwrap().effort(), Some("high"));
    assert_eq!(
        agent
            .reasoning_effort()
            .as_ref()
            .map(heycode_llm::ReasoningEffortId::as_str),
        Some("high")
    );
    world.shutdown();
}

#[tokio::test]
async fn opaque_checkpoint_requires_and_executes_portable_fork_or_cancel_explicitly() {
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(heycode_llm::testing::FakeProvider::new(vec![
            vec![
                heycode_llm::StreamChunk::TextDelta("portable summary".to_owned()),
                heycode_llm::StreamChunk::Finish(heycode_llm::FinishReason::Stop),
            ],
        ])));
    let sessions_root = harness.sessions_dir();
    let world = harness.compose().unwrap();
    let context = world.context();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    {
        let mut session = agent.session().lock().unwrap();
        let mut first_turn_end = None;
        for (turn, user, assistant) in [
            (0, "old question", "old answer"),
            (1, "recent question", "recent answer"),
        ] {
            session
                .append(heycode_session::SessionEventKind::UserMessage {
                    text: user.to_owned(),
                })
                .unwrap();
            session
                .append(heycode_session::SessionEventKind::TurnStart { turn })
                .unwrap();
            session
                .append(heycode_session::SessionEventKind::AssistantMessage {
                    turn,
                    step: 0,
                    content: assistant.to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                })
                .unwrap();
            let end = session
                .append(heycode_session::SessionEventKind::TurnEnd {
                    turn,
                    reason: heycode_session::TurnEndReason::Stop,
                })
                .unwrap();
            first_turn_end.get_or_insert(end.seq);
        }
        session
            .append(heycode_session::SessionEventKind::NativeCompactionApplied {
                strategy: "provider-native".to_owned(),
                replaced_upto_seq: first_turn_end.unwrap(),
                items: vec![opaque_checkpoint()],
                usage: None,
            })
            .unwrap();
    }

    let namespace = heycode_routing::settings_namespace().unwrap();
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let initial_revision = settings.get(&namespace).unwrap().unwrap().revision();
    let live_before = agent.selection();
    settings
        .replace_user(
            &namespace,
            serde_json::json!({
                "runtime":"native",
                "provider":"fake",
                "model":"external-model"
            }),
            Some(initial_revision),
        )
        .unwrap();
    assert_eq!(
        agent.selection(),
        live_before,
        "external Settings publication must not bypass the opaque barrier"
    );
    assert!(matches!(
        routing.select_provider("fake"),
        Err(heycode_routing::RoutingError::OpaqueStateResolutionRequired { .. })
    ));
    let revision = settings.get(&namespace).unwrap().unwrap().revision();
    let events_before = agent.session().lock().unwrap().events().len();
    let sessions_before = session_directory_count(&sessions_root);
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let provider = commands.get("provider").unwrap().unwrap();
    assert_eq!(provider.descriptor().arguments().len(), 2);

    provider.execute(&agent, "fake cancel").await.unwrap();
    assert_eq!(
        settings.get(&namespace).unwrap().unwrap().revision(),
        revision
    );
    assert_eq!(
        agent.session().lock().unwrap().events().len(),
        events_before
    );

    provider.execute(&agent, "fake fork").await.unwrap();
    assert_eq!(
        settings.get(&namespace).unwrap().unwrap().revision(),
        revision
    );
    assert_eq!(
        agent.session().lock().unwrap().events().len(),
        events_before
    );
    assert_eq!(session_directory_count(&sessions_root), sessions_before + 1);

    provider.execute(&agent, "fake portable").await.unwrap();
    assert_eq!(
        settings.get(&namespace).unwrap().unwrap().revision(),
        revision + 1
    );
    assert_eq!(
        agent.session().lock().unwrap().events().len(),
        events_before + 1
    );
    assert!(
        agent
            .provider_switch_barrier("fake", "fake-model")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        agent
            .session()
            .lock()
            .unwrap()
            .events()
            .last()
            .map(|event| &event.kind),
        Some(heycode_session::SessionEventKind::CompactionApplied {
            summary,
            replaced_upto_seq: _,
        }) if summary == "portable summary"
    ));
    world.shutdown();
}

#[tokio::test]
async fn a_model_flag_outranks_the_persisted_route_for_this_process_only() {
    let mut harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    std::fs::write(
        &settings_path,
        "[settings.routing]\nruntime = \"native\"\nprovider = \"fake\"\nmodel = \"persisted-model\"\n",
    )
    .unwrap();
    harness
        .config_mut()
        .apply_patch("llm.provider=fake")
        .unwrap();
    harness
        .config_mut()
        .apply_patch("llm.model=flag-model")
        .unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    assert_eq!(
        routing.selection().unwrap().model(),
        "flag-model",
        "the command line is the top ephemeral layer, like Claude Code's --model"
    );
    assert_eq!(agent.selection().model, "flag-model");
    assert_eq!(
        routing.override_notice(),
        Some(
            "command line sets model `flag-model` for this session (user settings have `persisted-model`)"
        )
    );
    assert!(
        std::fs::read_to_string(&settings_path)
            .unwrap()
            .contains("persisted-model"),
        "a flag never edits the persisted settings"
    );

    let selected = routing.select_model("fake-model").unwrap();
    assert_eq!(selected.model(), "fake-model");
    assert_eq!(
        agent.selection().model,
        "fake-model",
        "an in-session /model supersedes the flag instead of being silently ignored"
    );
    world.shutdown();
}

#[tokio::test]
async fn a_config_file_model_is_only_the_base_below_the_persisted_route() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        "[settings.routing]\nruntime = \"native\"\nprovider = \"fake\"\nmodel = \"persisted-model\"\n",
    )
    .unwrap();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    assert_eq!(routing.selection().unwrap().model(), "persisted-model");
    assert_eq!(routing.override_notice(), None);
    world.shutdown();
}

#[tokio::test]
async fn new_api_connection_is_durable_without_publishing_an_uncomposed_provider() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let before = routing.selection().unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "openai")
        .unwrap();
    routing
        .stage_connection("openai", profile.default_model.as_deref().unwrap(), None)
        .unwrap();
    assert_eq!(routing.selection().unwrap(), before);
    assert_eq!(agent.selection().provider_name, before.provider());
    let mut config = heycode_config::Config::default();
    config.llm.provider = "deepseek".into();
    config.llm.base_url = Some("https://old.example".into());
    config.llm.api_key_env = Some("OLD_API_KEY".into());
    let target =
        heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None).unwrap();
    assert_eq!(target.runtime(), "native");
    assert_eq!(config.llm.provider, "openai");
    assert_eq!(config.llm.model, profile.default_model.as_deref().unwrap());
    assert!(config.llm.base_url.is_none());
    assert!(config.llm.api_key_env.is_none());
    assert!(!config.is_patched("llm.provider"));
    assert!(
        routing
            .stage_connection("openai", "invented-model", None)
            .is_err()
    );
}

#[tokio::test]
async fn startup_connection_keeps_explicit_route_and_endpoint_overrides() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "openai")
        .unwrap();
    routing
        .stage_connection("openai", profile.default_model.as_deref().unwrap(), None)
        .unwrap();
    let mut config = heycode_config::Config::default();
    config.apply_patch("llm.provider=deepseek").unwrap();
    config.apply_patch("llm.model=explicit-model").unwrap();
    config
        .apply_patch("llm.base_url=https://custom.example")
        .unwrap();
    heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None).unwrap();
    assert_eq!(config.llm.provider, "deepseek");
    assert_eq!(config.llm.model, "explicit-model");
    assert_eq!(
        config.llm.base_url.as_deref(),
        Some("https://custom.example")
    );
}

#[tokio::test]
async fn selected_local_endpoint_and_model_restore_from_the_same_pending_commit() {
    for reference in [None, Some("HEYCODE_LOCAL_BOUND_KEY")] {
        let harness = RealCompositionHarness::new().unwrap();
        let settings_path = harness.settings_path();
        let world = harness.compose().unwrap();
        let routing = world
            .context()
            .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
            .unwrap();
        let profile = routing
            .connection_profiles()
            .iter()
            .find(|row| row.registry_name == "lmstudio")
            .unwrap();
        let catalog = heycode_llm::CatalogSnapshot {
            provider: profile.descriptor.clone(),
            models: vec![heycode_llm::ModelDescriptor::unknown("loaded-instance")],
            revision: 1,
            fetched_at_ms: 1,
        };
        routing
            .stage_connection_authenticated(
                "lmstudio",
                "loaded-instance",
                Some(&catalog),
                Some("http://localhost:2234"),
                reference
                    .map(|reference| {
                        heycode_credentials::CredentialReference::new(reference).unwrap()
                    })
                    .as_ref(),
            )
            .unwrap();
        assert_ne!(routing.selection().unwrap().provider(), "lmstudio");
        let mut config = heycode_config::Config::default();
        config.llm.api_key_env = Some("PREVIOUS_PROVIDER_KEY".into());
        heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None).unwrap();
        assert_eq!(config.llm.provider, "lmstudio");
        assert_eq!(config.llm.model, "loaded-instance");
        assert_eq!(
            config.llm.base_url.as_deref(),
            Some("http://localhost:2234")
        );
        assert_eq!(config.llm.api_key_env.as_deref(), reference);
    }
}

#[tokio::test]
async fn custom_server_profile_accepts_unknown_discovery_and_explicit_model_ids() {
    let harness = RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "custom-openai")
        .unwrap();
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Local);
    assert!(profile.default_endpoint.is_none());
    assert!(profile.default_model.is_none());
    assert!(profile.credential_reference.is_none());
    assert!(profile.allows_explicit_model());
    let unknown = heycode_llm::ModelDescriptor::unknown("local/model");
    assert_eq!(
        unknown.capabilities.tools,
        heycode_llm::CapabilitySupport::Unknown
    );
    assert!(profile.admits_discovered_model(&unknown));
    let models = world
        .context()
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        models
            .supports_endpoint_credentials("custom-openai")
            .unwrap()
    );
    world.shutdown();
}

#[tokio::test]
async fn explicit_custom_server_route_restores_url_model_and_optional_reference_atomically() {
    for reference in [None, Some("HEYCODE_LOCAL_BOUND_KEY")] {
        let harness = RealCompositionHarness::new().unwrap();
        let settings_path = harness.settings_path();
        let world = harness.compose().unwrap();
        let routing = world
            .context()
            .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
            .unwrap();
        let candidate = heycode_routing::RoutingSelection::new(
            "native",
            "custom-openai",
            "local/model:latest",
            None,
        )
        .unwrap()
        .with_endpoint(Some("http://localhost:8000/v1".to_owned()))
        .unwrap()
        .with_credential_reference(
            reference.map(|value| heycode_credentials::CredentialReference::new(value).unwrap()),
        );
        routing
            .stage_connection_selection(&candidate, None)
            .unwrap();

        let mut config = heycode_config::Config::default();
        config.llm.api_key_env = Some("PREVIOUS_PROVIDER_KEY".into());
        let restored =
            heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None)
                .unwrap();
        assert_eq!(restored, candidate);
        assert_eq!(config.llm.provider, "custom-openai");
        assert_eq!(config.llm.model, "local/model:latest");
        assert_eq!(
            config.llm.base_url.as_deref(),
            Some("http://localhost:8000/v1")
        );
        assert_eq!(config.llm.api_key_env.as_deref(), reference);
        world.shutdown();
    }
}

#[test]
fn saved_custom_server_route_composes_catalog_inference_and_agent_without_a_key() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "deepseek"
model = "deepseek-v4-flash"
pending_connection = { provider = "custom-openai", model = "local/model:latest", endpoint = "http://localhost:8000/v1", parameters = {} }
"#,
    )
    .unwrap();
    let world = harness.without_fake_provider().compose().unwrap();
    let context = world.context();
    assert!(context.plugins().contains(&"catalog-custom-openai"));
    assert!(context.plugins().contains(&"inference-custom-openai"));
    let models = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        models
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor.id == "custom-openai")
    );
    let providers = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let provider = providers.get("custom-openai").unwrap();
    assert_eq!(provider.info().default_model, "local/model:latest");
    assert_eq!(provider.credential_reference(), None);
    assert_eq!(
        provider
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        heycode_llm::AuthenticationBinding::None
    );
    let selection = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap()
        .selection()
        .unwrap();
    assert_eq!(selection.provider(), "custom-openai");
    assert_eq!(selection.model(), "local/model:latest");
    assert_eq!(selection.endpoint(), Some("http://localhost:8000/v1"));
    assert!(selection.credential_reference().is_none());
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert_eq!(agent.selection().provider_name, "custom-openai");
    assert_eq!(agent.selection().model, "local/model:latest");
    world.shutdown();
}

#[test]
fn saved_custom_server_key_reference_binds_catalog_and_inference_without_resolving_it() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "deepseek"
model = "deepseek-v4-flash"
pending_connection = { provider = "custom-openai", model = "secured-model", endpoint = "https://server.example/v1", credential_reference = "HEYCODE_CUSTOM_SELECTED_KEY", parameters = {} }
"#,
    )
    .unwrap();
    let world = harness
        .without_fake_provider()
        .with_onboarding_required()
        .compose()
        .unwrap();
    let context = world.context();
    let provider = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .get("custom-openai")
        .unwrap();
    assert_eq!(
        provider.credential_reference(),
        Some("HEYCODE_CUSTOM_SELECTED_KEY")
    );
    assert_eq!(
        provider
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("HEYCODE_CUSTOM_SELECTED_KEY").unwrap()
        )
    );
    let models = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        models
            .supports_endpoint_credentials("custom-openai")
            .unwrap()
    );
    world.shutdown();
}

#[tokio::test]
async fn bedrock_is_a_cloud_connection_with_a_required_region_and_no_invented_model() {
    let harness = RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "bedrock")
        .unwrap();
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Cloud);
    assert_eq!(profile.default_model, None);
    assert_eq!(profile.parameters.len(), 1);
    assert_eq!(profile.parameters[0].id, "region");
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE)
    );
    world.shutdown();
}

#[tokio::test]
async fn selected_bedrock_coordinates_and_reference_restore_together() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "bedrock")
        .unwrap();
    let model = "anthropic.claude-test-v1:0";
    let catalog = heycode_llm::CatalogSnapshot {
        provider: profile.descriptor.clone(),
        models: vec![heycode_llm::ModelDescriptor::unknown(model)],
        revision: 1,
        fetched_at_ms: u64::MAX,
    };
    let reference =
        heycode_credentials::CredentialReference::new("HEYCODE_BEDROCK_SELECTED_KEY").unwrap();
    let candidate = heycode_routing::RoutingSelection::new("native", "bedrock", model, None)
        .unwrap()
        .with_parameters(std::collections::BTreeMap::from([(
            "region".into(),
            "ap-southeast-2".into(),
        )]))
        .unwrap()
        .with_credential_reference(Some(reference));
    routing
        .stage_connection_selection(&candidate, Some(&catalog))
        .unwrap();

    let mut config = heycode_config::Config::default();
    let restored =
        heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None).unwrap();
    assert_eq!(restored, candidate);
    assert_eq!(config.llm.provider, "bedrock");
    assert_eq!(config.llm.model, model);
    assert_eq!(
        config.llm.api_key_env.as_deref(),
        Some("HEYCODE_BEDROCK_SELECTED_KEY")
    );
    assert!(config.llm.base_url.is_none());
    world.shutdown();
}

#[test]
fn saved_bedrock_route_composes_catalog_authorization_and_inference_on_one_coordinate_tuple() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "deepseek"
model = "deepseek-v4-flash"
pending_connection = { provider = "bedrock", model = "anthropic.claude-test-v1:0", credential_reference = "HEYCODE_BEDROCK_SELECTED_KEY", parameters = { region = "ap-southeast-2" } }
"#,
    )
    .unwrap();
    let world = harness
        .without_fake_provider()
        .with_onboarding_required()
        .compose()
        .unwrap();
    let context = world.context();
    assert!(context.plugins().contains(&"inference-bedrock-converse"));
    let authorization = context
        .get::<heycode_authorization::AuthorizationService>(
            heycode_authorization::SERVICE_AUTHORIZATION,
        )
        .unwrap();
    let flow = authorization
        .descriptors()
        .unwrap()
        .into_iter()
        .find(|flow| flow.id.as_str() == heycode_authorization_aws::AWS_BEDROCK_FLOW_ID)
        .unwrap();
    assert_eq!(
        flow.query.reference.as_str(),
        "HEYCODE_BEDROCK_SELECTED_KEY"
    );
    let aws = context
        .get::<heycode_authorization_aws::AwsAuthService>(
            heycode_authorization_aws::SERVICE_AWS_AUTH,
        )
        .unwrap();
    assert_eq!(
        aws.region()
            .region()
            .map(heycode_authorization_aws::AwsRegion::as_str),
        Some("ap-southeast-2")
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert_eq!(agent.selection().provider_name, "bedrock");
    assert_eq!(agent.selection().model, "anthropic.claude-test-v1:0");
    world.shutdown();
}

#[tokio::test]
async fn vertex_is_a_cloud_connection_with_exact_external_authority_metadata() {
    let harness = RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "vertex-google")
        .unwrap();
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Cloud);
    assert_eq!(
        profile.default_model.as_deref(),
        Some(heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH)
    );
    assert_eq!(
        profile
            .parameters
            .iter()
            .map(|parameter| parameter.id.as_str())
            .collect::<Vec<_>>(),
        ["project", "location"]
    );
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE)
    );
    let models = world
        .context()
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        !models
            .supports_parameter_credentials("vertex-google")
            .unwrap()
    );
    world.shutdown();
}

#[tokio::test]
async fn selected_vertex_coordinates_and_reference_restore_atomically() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "vertex-google")
        .unwrap();
    let model = heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH;
    let catalog = heycode_llm::CatalogSnapshot {
        provider: profile.descriptor.clone(),
        models: vec![heycode_llm::ModelDescriptor::unknown(model)],
        revision: 1,
        fetched_at_ms: u64::MAX,
    };
    let reference =
        heycode_credentials::CredentialReference::new("HEYCODE_VERTEX_SELECTED_TOKEN").unwrap();
    let candidate = heycode_routing::RoutingSelection::new("native", "vertex-google", model, None)
        .unwrap()
        .with_parameters(std::collections::BTreeMap::from([
            ("location".into(), "us-central1".into()),
            ("project".into(), "vertex-fixture".into()),
        ]))
        .unwrap()
        .with_credential_reference(Some(reference));
    routing
        .stage_connection_selection(&candidate, Some(&catalog))
        .unwrap();

    let mut config = heycode_config::Config::default();
    let restored =
        heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None).unwrap();
    assert_eq!(restored, candidate);
    assert_eq!(config.llm.provider, "vertex-google");
    assert_eq!(config.llm.model, model);
    assert_eq!(
        config.llm.api_key_env.as_deref(),
        Some("HEYCODE_VERTEX_SELECTED_TOKEN")
    );
    assert!(config.llm.base_url.is_none());
    world.shutdown();
}

#[test]
fn saved_vertex_route_composes_one_reachable_readiness_source_and_inference_route() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        format!(
            r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "deepseek"
model = "deepseek-v4-flash"
pending_connection = {{ provider = "vertex-google", model = "{}", credential_reference = "HEYCODE_VERTEX_SELECTED_TOKEN", parameters = {{ project = "vertex-fixture", location = "us-central1" }} }}
"#,
            heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH
        ),
    )
    .unwrap();
    let world = harness
        .without_fake_provider()
        .with_onboarding_required()
        .compose()
        .unwrap();
    let context = world.context();
    assert!(context.plugins().contains(&"inference-google-vertex"));
    assert_eq!(
        context
            .plugins()
            .iter()
            .filter(|plugin| **plugin == "inference-google-vertex")
            .count(),
        1
    );
    let models = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        models
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor.id == "vertex-google")
    );
    assert!(
        !models
            .supports_parameter_credentials("vertex-google")
            .unwrap()
    );
    let routing = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let selection = routing.selection().unwrap();
    assert_eq!(selection.provider(), "vertex-google");
    assert_eq!(
        selection.model(),
        heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH
    );
    assert_eq!(
        selection.parameters().get("project").map(String::as_str),
        Some("vertex-fixture")
    );
    assert_eq!(
        selection.parameters().get("location").map(String::as_str),
        Some("us-central1")
    );
    assert_eq!(
        selection
            .credential_reference()
            .map(heycode_credentials::CredentialReference::as_str),
        Some("HEYCODE_VERTEX_SELECTED_TOKEN")
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert_eq!(agent.selection().provider_name, "vertex-google");
    assert_eq!(
        agent.selection().model,
        heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH
    );
    world.shutdown();
}

#[tokio::test]
async fn azure_is_a_cloud_connection_with_exact_resource_deployment_and_masked_key_metadata() {
    let harness = RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "azure-openai")
        .unwrap();
    assert_eq!(profile.family, heycode_llm::ConnectionFamily::Cloud);
    assert_eq!(profile.default_model, None);
    assert_eq!(
        profile
            .parameters
            .iter()
            .map(|parameter| parameter.id.as_str())
            .collect::<Vec<_>>(),
        ["resource", "deployment"]
    );
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(heycode_provider_azure::AZURE_OPENAI_API_KEY_REFERENCE)
    );
    let models = world
        .context()
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        models
            .supports_parameter_credentials("azure-openai")
            .unwrap()
    );
    world.shutdown();
}

#[tokio::test]
async fn selected_azure_coordinates_deployment_and_reference_restore_atomically() {
    let harness = RealCompositionHarness::new().unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "azure-openai")
        .unwrap();
    let catalog = heycode_llm::CatalogSnapshot {
        provider: profile.descriptor.clone(),
        models: vec![heycode_llm::ModelDescriptor::unknown("prod-gpt")],
        revision: 1,
        fetched_at_ms: u64::MAX,
    };
    let reference =
        heycode_credentials::CredentialReference::new("HEYCODE_AZURE_SELECTED_KEY").unwrap();
    let candidate =
        heycode_routing::RoutingSelection::new("native", "azure-openai", "prod-gpt", None)
            .unwrap()
            .with_parameters(std::collections::BTreeMap::from([
                ("deployment".into(), "prod-gpt".into()),
                ("resource".into(), "team-agent".into()),
            ]))
            .unwrap()
            .with_credential_reference(Some(reference));
    routing
        .stage_connection_selection(&candidate, Some(&catalog))
        .unwrap();

    let mut config = heycode_config::Config::default();
    let restored =
        heycode_cli::apply_startup_connection_from_files(&mut config, settings_path, None).unwrap();
    assert_eq!(restored, candidate);
    assert_eq!(config.llm.provider, "azure-openai");
    assert_eq!(config.llm.model, "prod-gpt");
    assert_eq!(
        config.llm.api_key_env.as_deref(),
        Some("HEYCODE_AZURE_SELECTED_KEY")
    );
    assert!(config.llm.base_url.is_none());
    world.shutdown();
}

#[test]
fn saved_azure_route_composes_the_same_catalog_provider_and_connection_tuple() {
    let harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "deepseek"
model = "deepseek-v4-flash"
pending_connection = { provider = "azure-openai", model = "prod-gpt", credential_reference = "HEYCODE_AZURE_SELECTED_KEY", parameters = { resource = "team-agent", deployment = "prod-gpt" } }
"#,
    )
    .unwrap();
    let world = harness
        .without_fake_provider()
        .with_onboarding_required()
        .compose()
        .unwrap();
    let context = world.context();
    assert!(context.plugins().contains(&"inference-azure-openai"));
    let models = context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    assert!(
        models
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor.id == "azure-openai")
    );
    assert!(
        models
            .supports_parameter_credentials("azure-openai")
            .unwrap()
    );
    let providers = context
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let provider = providers.get("azure-openai").unwrap();
    assert_eq!(provider.info().default_model, "prod-gpt");
    assert!(provider.inference_adapter().is_some());
    let selection = context
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap()
        .selection()
        .unwrap();
    assert_eq!(selection.provider(), "azure-openai");
    assert_eq!(selection.model(), "prod-gpt");
    assert_eq!(
        selection.parameters().get("resource").map(String::as_str),
        Some("team-agent")
    );
    assert_eq!(
        selection.parameters().get("deployment").map(String::as_str),
        Some("prod-gpt")
    );
    assert_eq!(
        selection
            .credential_reference()
            .map(heycode_credentials::CredentialReference::as_str),
        Some("HEYCODE_AZURE_SELECTED_KEY")
    );
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert_eq!(agent.selection().provider_name, "azure-openai");
    assert_eq!(agent.selection().model, "prod-gpt");
    world.shutdown();
}

#[test]
fn startup_connection_report_attributes_saved_and_pinned_models() {
    use heycode_config::ConfigValueSource;
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("settings.toml");
    let project = dir.path().join("project.toml");
    std::fs::write(
        &user,
        "[settings.routing]\nprovider = 'openrouter'\nmodel = 'saved-user-model'\n",
    )
    .unwrap();
    std::fs::write(
        &project,
        "[settings.routing]\nmodel = 'saved-project-model'\n",
    )
    .unwrap();
    for (project_path, pin, expected_model, expected_source) in [
        (
            None,
            false,
            "saved-user-model",
            ConfigValueSource::Settings("user settings (routing.model)".into()),
        ),
        (
            Some(project.clone()),
            false,
            "saved-project-model",
            ConfigValueSource::Settings("project settings (routing.model)".into()),
        ),
        (Some(project), true, "pinned-model", ConfigValueSource::Flag),
    ] {
        let mut config = heycode_config::Config::default();
        config.llm.provider = "deepseek".into();
        config.llm.model = "legacy-model".into();
        config.llm.base_url = Some("https://old.example".into());
        if pin {
            config.apply_patch("llm.model=pinned-model").unwrap();
        }
        heycode_cli::apply_startup_connection_from_files(&mut config, user.clone(), project_path)
            .unwrap();
        assert_eq!(config.llm.model, expected_model);
        assert!(config.llm.base_url.is_none());
        let report = config.report();
        let model = report
            .rows()
            .iter()
            .find(|row| row.key == "llm.model")
            .unwrap();
        assert_eq!(model.source, expected_source);
        assert!(model.value.contains(expected_model));
    }
    std::fs::write(
        &user,
        "[settings.routing]\nsetup_required = true\nprovider = 'openrouter'\nmodel = 'logged-out-model'\n",
    )
    .unwrap();
    let mut config = heycode_config::Config::default();
    let original = config.llm.model.clone();
    heycode_cli::apply_startup_connection_from_files(&mut config, user, None).unwrap();
    assert_eq!(config.llm.model, original);
    assert_eq!(
        config
            .report()
            .rows()
            .iter()
            .find(|row| row.key == "llm.model")
            .unwrap()
            .source,
        ConfigValueSource::Default
    );
}

#[test]
fn config_show_uses_admitted_route_without_activation_or_project_trust() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(workspace.join(".heycode")).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[llm]\nprovider = 'openrouter'\nmodel = 'legacy-model'\n",
    )
    .unwrap();
    std::fs::write(
        home.join("settings.toml"),
        "[settings.routing]\nprovider = 'openrouter'\nmodel = 'saved-user-model'\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join(".heycode/settings.toml"),
        "[settings.routing]\nmodel = 'untrusted-project-model'\n",
    )
    .unwrap();
    for pin in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"));
        command
            .env("HEYCODE_HOME", &home)
            .current_dir(&workspace)
            .arg("--restricted-workspace");
        if pin {
            command.args(["--model", "pinned-model"]);
        }
        let output = command.args(["config", "show"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let model = stdout
            .lines()
            .find(|line| line.contains("llm.model"))
            .unwrap();
        assert!(
            model.contains(if pin {
                "pinned-model"
            } else {
                "saved-user-model"
            }),
            "{model}"
        );
        assert!(
            model.contains(if pin {
                "command line"
            } else {
                "user settings (routing.model)"
            }),
            "{model}"
        );
        assert!(!stdout.contains("untrusted-project-model"));
        assert!(stdout.contains("runtime = native"));
    }
    assert!(!home.join("sessions").exists());
}

#[tokio::test]
async fn model_session_selection_is_live_without_changing_saved_defaults() {
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(EffortAwareProvider));
    harness
        .seed_catalog_snapshot(heycode_llm::CatalogSnapshot {
            provider: effort_provider_descriptor(),
            models: vec![
                heycode_llm::Provider::describe_model(&EffortAwareProvider, "fake-model"),
                heycode_llm::Provider::describe_model(&EffortAwareProvider, "fake-other"),
            ],
            revision: 1,
            fetched_at_ms: 1,
        })
        .unwrap();
    let settings_path = harness.settings_path();
    let world = harness.compose().unwrap();
    struct RoutingCatalog;
    #[async_trait::async_trait]
    impl heycode_llm::ModelCatalog for RoutingCatalog {
        fn provider(&self) -> heycode_llm::ProviderDescriptor {
            effort_provider_descriptor()
        }
        async fn fetch(
            &self,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Result<Vec<heycode_llm::ModelDescriptor>, heycode_llm::CatalogFetchError> {
            if cancellation.is_cancelled() {
                return Err(heycode_llm::CatalogFetchError::new(
                    heycode_llm::CatalogFailureKind::Cancelled,
                    "cancelled",
                ));
            }
            Ok(vec![
                heycode_llm::Provider::describe_model(&EffortAwareProvider, "fake-model"),
                heycode_llm::Provider::describe_model(&EffortAwareProvider, "fake-other"),
            ])
        }
    }
    world
        .context()
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap()
        .register(world.context(), Arc::new(RoutingCatalog))
        .unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    routing.select_provider("fake").unwrap();
    let before = std::fs::read(&settings_path).unwrap();
    let active = routing.active_configuration().unwrap();
    let catalog = routing
        .models_owned(
            active.owner(),
            heycode_llm::CatalogRefreshMode::PreferCache,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    routing
        .select_model_configuration_owned(
            active.owner(),
            active.revision(),
            Some(catalog.snapshot.as_ref()),
            heycode_routing::ModelControlChoice {
                model: "fake-other",
                effort: Some("high"),
                scope: heycode_routing::SelectionScope::Session,
            },
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(routing.selection().unwrap().model(), "fake-other");
    assert_eq!(routing.selection().unwrap().effort(), Some("high"));
    assert_eq!(std::fs::read(&settings_path).unwrap(), before);
    assert!(
        routing
            .select_model_owned_in_scope(
                active.owner(),
                active.revision(),
                Some(catalog.snapshot.as_ref()),
                "fake-model",
                heycode_routing::SelectionScope::Session,
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    let active = routing.active_configuration().unwrap();
    let choices = routing
        .model_effort_catalog_owned(
            active.owner(),
            active.revision(),
            "fake-model",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(choices.choices(), ["low", "high"]);
    assert_eq!(
        choices.current(),
        None,
        "a highlighted alternative must not borrow the active model's effort"
    );
    assert!(
        routing
            .select_model_configuration_owned(
                active.owner(),
                active.revision(),
                Some(catalog.snapshot.as_ref()),
                heycode_routing::ModelControlChoice {
                    model: "fake-model",
                    effort: Some("unsupported"),
                    scope: heycode_routing::SelectionScope::Session
                },
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        routing.active_configuration().unwrap().revision(),
        active.revision()
    );
    assert_eq!(routing.selection().unwrap().effort(), Some("high"));
    assert!(
        routing
            .select_model_owned_in_scope(
                active.owner(),
                active.revision(),
                Some(catalog.snapshot.as_ref()),
                "not-in-catalog",
                heycode_routing::SelectionScope::Session,
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(routing.selection().unwrap().model(), "fake-other");
    assert_eq!(std::fs::read(&settings_path).unwrap(), before);
    world.shutdown();
    let restore_harness = RealCompositionHarness::new().unwrap();
    std::fs::write(restore_harness.settings_path(), before).unwrap();
    let restored = restore_harness.compose().unwrap();
    let routing = restored
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    assert_eq!(routing.selection().unwrap().model(), "fake-model");
    restored.shutdown();
}

/// No-op transport: this journey never dispatches inference or catalog I/O.
struct OfflineTransport;

impl heycode_http::HttpTransport for OfflineTransport {
    fn send(
        &self,
        _request: heycode_http::HttpRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> heycode_http::BufferedResponseFuture {
        Box::pin(async { Err(heycode_http::TransportError::Timeout) })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> heycode_http::SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn openrouter_route() -> Arc<heycode_llm::OpenRouterProvider> {
    Arc::new(
        heycode_llm::OpenRouterProvider::from_key_with_transport(
            "offline-fixture-key",
            Some(heycode_llm::OpenRouterProvider::DEFAULT_MODEL.to_owned()),
            heycode_http::HttpService::new(Arc::new(OfflineTransport)),
            vec![
                heycode_core::ProviderRequestOption::new(
                    "openrouter",
                    "transforms",
                    serde_json::json!({"plugins": []}),
                )
                .unwrap(),
            ],
        )
        .unwrap(),
    )
}

fn openrouter_row(
    id: &str,
    reasoning: Option<heycode_llm::ModelReasoningMetadata>,
) -> heycode_llm::ModelDescriptor {
    let mut descriptor = heycode_llm::ModelDescriptor::unknown(id);
    descriptor.capabilities.reasoning = heycode_llm::CapabilitySupport::Supported;
    descriptor.lifecycle = heycode_llm::ModelLifecycle::stable();
    descriptor.reasoning = reasoning;
    descriptor
}

fn published(
    efforts: &[&str],
    default_effort: &str,
) -> Option<heycode_llm::ModelReasoningMetadata> {
    Some(
        heycode_llm::ModelReasoningMetadata::published(
            efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            Some(default_effort.to_owned()),
            Some(true),
            Some(false),
        )
        .unwrap(),
    )
}

fn openrouter_catalog_models() -> Vec<heycode_llm::ModelDescriptor> {
    vec![
        openrouter_row(
            heycode_llm::OpenRouterProvider::DEFAULT_MODEL,
            published(&["max", "high", "low"], "max"),
        ),
        openrouter_row(
            "vendor/graded-reasoner",
            published(&["low", "medium", "high"], "medium"),
        ),
        openrouter_row("vendor/silent-reasoner", None),
    ]
}

#[tokio::test]
async fn openrouter_effort_controls_follow_each_model_published_vocabulary() {
    let provider_descriptor = heycode_llm::Provider::descriptor(openrouter_route().as_ref());
    // The composed HTTP service is pinned to a transport that reaches nothing,
    // so this journey can only read the seeded catalog cache.
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_http_transport(Arc::new(OfflineTransport))
        .with_provider(openrouter_route());
    harness
        .seed_catalog_snapshot(heycode_llm::CatalogSnapshot {
            provider: provider_descriptor.clone(),
            models: openrouter_catalog_models(),
            revision: 1,
            fetched_at_ms: 1,
        })
        .unwrap();
    // Composition already owns the real OpenRouter catalog source; this journey
    // reads the seeded cache and never reaches the network.
    let world = harness.compose().unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    routing.select_provider("openrouter").unwrap();

    // The verified strict route keeps exactly the list it published.
    let options = routing.effort_options().unwrap().unwrap();
    assert_eq!(
        options
            .choices()
            .iter()
            .map(heycode_llm::ReasoningEffortId::as_str)
            .collect::<Vec<_>>(),
        ["max", "high", "low"]
    );

    // Highlighting another model loads that model's own vocabulary, not the
    // active route's.
    let active = routing.active_configuration().unwrap();
    let catalog = routing
        .models_owned(
            active.owner(),
            heycode_llm::CatalogRefreshMode::PreferCache,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let graded = routing
        .model_effort_catalog_owned(
            active.owner(),
            active.revision(),
            "vendor/graded-reasoner",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(graded.choices(), ["low", "medium", "high"]);
    assert_eq!(graded.default(), Some("medium"));

    // A reasoning model that published no vocabulary offers no effort control.
    assert!(
        routing
            .model_effort_catalog_owned(
                active.owner(),
                active.revision(),
                "vendor/silent-reasoner",
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .is_err(),
        "an unpublished vocabulary must not fall back to the strict route list"
    );

    // `max` is the strict route's value; the graded model never accepts it.
    assert!(
        routing
            .select_model_configuration_owned(
                active.owner(),
                active.revision(),
                Some(catalog.snapshot.as_ref()),
                heycode_routing::ModelControlChoice {
                    model: "vendor/graded-reasoner",
                    effort: Some("max"),
                    scope: heycode_routing::SelectionScope::Session,
                },
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        routing.selection().unwrap().model(),
        heycode_llm::OpenRouterProvider::DEFAULT_MODEL
    );

    routing
        .select_model_configuration_owned(
            active.owner(),
            active.revision(),
            Some(catalog.snapshot.as_ref()),
            heycode_routing::ModelControlChoice {
                model: "vendor/graded-reasoner",
                effort: Some("medium"),
                scope: heycode_routing::SelectionScope::Session,
            },
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(routing.selection().unwrap().effort(), Some("medium"));
    world.shutdown();
}

#[tokio::test]
async fn a_saved_effort_the_model_no_longer_publishes_starts_unset_with_a_receipt() {
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_http_transport(Arc::new(OfflineTransport));
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1

[settings.routing]
runtime = "native"
provider = "openrouter"
model = "vendor/graded-reasoner"
effort = "max"
"#,
    )
    .unwrap();
    let provider_descriptor = heycode_llm::Provider::descriptor(openrouter_route().as_ref());
    harness
        .seed_catalog_snapshot(heycode_llm::CatalogSnapshot {
            provider: provider_descriptor,
            models: openrouter_catalog_models(),
            revision: 1,
            fetched_at_ms: 1,
        })
        .unwrap();
    let saved = std::fs::read(harness.settings_path()).unwrap();
    let settings_path = harness.settings_path();
    let world = harness.with_provider(openrouter_route()).compose().unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();

    // `max` belongs to the strict GLM route, never to this model. The live
    // route starts without an effort rather than sending a value the model
    // does not publish or coercing it into a neighbouring one.
    assert_eq!(agent.reasoning_effort(), None);
    // The saved file is left exactly as the operator wrote it.
    assert_eq!(std::fs::read(&settings_path).unwrap(), saved);
    assert_eq!(routing.selection().unwrap().effort(), Some("max"));
    world.shutdown();
}

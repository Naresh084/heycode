//! QSEC02 one-canary product journey across every non-egress surface.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{Context, CoreError, Plugin};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpSseRequest, HttpTransport, SseEvent, SseEventStream,
    TransportError,
};
use heycode_llm::{
    CapabilitySupport, CatalogFetchError, CatalogRegistry, DeepSeekProvider, LlmSelection,
    ModelCapabilities, ModelCatalog, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, Provider, ProviderDescriptor, RouteCredential,
};
use tokio_util::sync::CancellationToken;

const CANARY: &str = "qsec02-env-secret-canary-7f1e";
const REFERENCE: &str = "HEYCODE_QSEC02_ENV_KEY";

struct CanaryTransport {
    authorized: std::sync::atomic::AtomicBool,
    body: Mutex<Vec<u8>>,
}

impl CanaryTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            authorized: std::sync::atomic::AtomicBool::new(false),
            body: Mutex::new(Vec::new()),
        })
    }

    fn authorized(&self) -> bool {
        self.authorized.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl HttpTransport for CanaryTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        let expected = format!("Bearer {CANARY}");
        self.authorized.store(
            request.headers().iter().any(|header| {
                header.name().eq_ignore_ascii_case("authorization") && header.value() == expected
            }),
            std::sync::atomic::Ordering::SeqCst,
        );
        *self.body.lock().unwrap() = request.body().unwrap_or_default().to_vec();
        Box::pin(futures::stream::iter([
            Ok(SseEvent {
                event: "message".to_owned(),
                data: serde_json::json!({
                    "id":"qsec02-response",
                    "choices":[{
                        "index":0,
                        "delta":{"content":"safe answer"},
                        "finish_reason":"stop"
                    }],
                    "usage":{"prompt_tokens":3,"completion_tokens":2}
                })
                .to_string(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".to_owned(),
                data: "[DONE]".to_owned(),
                id: None,
                retry_ms: None,
            }),
        ]))
    }

    fn send(
        &self,
        _request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        Box::pin(async {
            Err(TransportError::Network {
                message: "unexpected buffered request".to_owned(),
            })
        })
    }
}

struct StaticCatalog {
    provider: ProviderDescriptor,
    model: ModelDescriptor,
}

#[async_trait]
impl ModelCatalog for StaticCatalog {
    fn provider(&self) -> ProviderDescriptor {
        self.provider.clone()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        Ok(vec![self.model.clone()])
    }
}

struct CatalogPlugin(Arc<dyn ModelCatalog>);

impl Plugin for CatalogPlugin {
    fn name(&self) -> &'static str {
        "qsec02-catalog"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            self.name(),
            env!("CARGO_PKG_VERSION"),
            &[heycode_core::PluginContributionKind::Provider],
        )
    }

    fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
        vec![heycode_core::PluginContributionSpec::new(
            heycode_core::ContributionKind::ModelCatalog,
            DeepSeekProvider::NAME,
        )]
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[heycode_llm::SERVICE_MODELS]
    }

    fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
        let catalogs = context
            .get::<CatalogRegistry>(heycode_llm::SERVICE_MODELS)
            .ok_or_else(|| CoreError::other("model catalog service missing"))?;
        catalogs
            .register(context, self.0.clone())
            .map_err(|error| CoreError::other(error.to_string()))
    }
}

fn deepseek_model() -> ModelDescriptor {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.reasoning = CapabilitySupport::Supported;
    capabilities.prompt_cache = CapabilitySupport::Supported;
    ModelDescriptor {
        id: DeepSeekProvider::DEFAULT_MODEL.to_owned(),
        display_name: "QSEC02 DeepSeek fixture".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(393_216),
        lifecycle: ModelLifecycle::preview(),
        capabilities,
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(REFERENCE).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn assert_absent(surface: &str, label: &str) {
    assert!(
        !surface.contains(CANARY),
        "{label} leaked the canary: {surface}"
    );
}

#[tokio::test]
async fn environment_credential_reaches_only_auth_and_never_log_session_prompt_or_support() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().canonicalize().unwrap();
    let sessions_root = root.path().join("sessions");

    let credentials = CredentialsService::new();
    let mut credential_context = Context::new();
    credentials
        .register(
            &credential_context,
            Arc::new(
                heycode_credentials_env::EnvironmentCredentialProvider::from_map(BTreeMap::from([
                    (REFERENCE.to_owned(), CANARY.to_owned()),
                ]))
                .unwrap(),
            ),
        )
        .unwrap();
    let descriptor = credentials.describe(&query()).unwrap();
    assert_eq!(
        descriptor.source,
        Some(heycode_credentials::CredentialSource::Environment)
    );
    let descriptor_json = serde_json::to_string(&descriptor).unwrap();
    assert_absent(&descriptor_json, "credential descriptor");
    let resolved = credentials.resolve_route(&query()).unwrap();
    assert_eq!(
        resolved.expose(),
        CANARY,
        "premise: environment value resolves"
    );
    let secret_debug = format!("{resolved:?}");
    assert_absent(&secret_debug, "credential Debug");
    drop(resolved);

    let transport = CanaryTransport::new();
    let credential = RouteCredential::registry(credentials.clone(), query());
    let route_debug = format!("{credential:?}");
    assert_absent(&route_debug, "route credential Debug");
    let provider = Arc::new(
        DeepSeekProvider::from_credential_with_transport(
            credential,
            Some(DeepSeekProvider::DEFAULT_MODEL.to_owned()),
            heycode_http::HttpService::new(transport.clone()),
        )
        .unwrap(),
    );
    let provider_descriptor = Provider::descriptor(provider.as_ref());
    let catalog: Arc<dyn ModelCatalog> = Arc::new(StaticCatalog {
        provider: provider_descriptor,
        model: deepseek_model(),
    });

    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(sessions_root.clone()),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        Box::new(CatalogPlugin(catalog)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            LlmSelection {
                provider_name: DeepSeekProvider::NAME.to_owned(),
                model: DeepSeekProvider::DEFAULT_MODEL.to_owned(),
            },
            vec![provider],
        ),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::AutoApprove)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::agent_options_plugin(heycode_agent::AgentOptions {
            cwd: Some(cwd.clone()),
            ..heycode_agent::AgentOptions::default()
        }),
        heycode_agent::agent_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let ui_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_ui = ui_log.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        captured_ui.lock().unwrap().push(format!("{event:?}"));
    });
    let report = agent.send("public qsec02 prompt").await.unwrap();
    assert_eq!(report.text, "safe answer");
    assert!(
        transport.authorized(),
        "premise: canary reached only the auth header"
    );

    let request_body = String::from_utf8(transport.body.lock().unwrap().clone()).unwrap();
    assert_absent(&request_body, "provider request body/prompt");
    let ui_text = ui_log.lock().unwrap().join("\n");
    assert_absent(&ui_text, "UI/log plane");
    let (session_id, session_path, event_debug, request_debug) = {
        let session = agent
            .session()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let projected = heycode_session::project_requests(session.events()).unwrap();
        (
            session.id().clone(),
            session.path().to_path_buf(),
            format!("{:?}", session.events()),
            format!("{:?}", projected),
        )
    };
    assert_absent(&event_debug, "session event Debug");
    assert_absent(&request_debug, "request projection/prompt");
    let session_jsonl = std::fs::read_to_string(&session_path).unwrap();
    assert_absent(&session_jsonl, "physical session log");

    let process = heycode_exec::ProcessSpec::new("/usr/bin/env", cwd)
        .unwrap()
        .with_args([OsString::from(CANARY)])
        .unwrap()
        .with_environment([(OsString::from("QSEC02_SECRET"), OsString::from(CANARY))])
        .unwrap();
    assert!(process.args()[0].to_string_lossy().contains(CANARY));
    assert!(
        process.environment()[0]
            .1
            .to_string_lossy()
            .contains(CANARY)
    );
    assert_absent(&format!("{process:?}"), "process/log Debug");

    context.shutdown();
    drop(agent);
    drop(context);
    credential_context.shutdown();
    drop(credential_context);

    let receipt = heycode_session::SessionQueryService::local(sessions_root)
        .export(
            &session_id,
            heycode_session::SessionExportFormat::RedactedSupport,
        )
        .unwrap();
    let support = std::fs::read_to_string(receipt.path()).unwrap();
    assert_absent(&support, "redacted support bundle");
}

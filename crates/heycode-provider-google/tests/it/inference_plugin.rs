//! Effect-owned Google inference construction and native candidate ownership.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_authorization_gcp::{
    ENV_GOOGLE_APPLICATION_CREDENTIALS, GcpAuthService, GcpHealth, GcpHostPlatform,
    GcpMetadataPolicy, GcpProfileRequest, SERVICE_GCP_AUTH,
};
use heycode_core::{Context, ContributionKind, CoreError, Plugin, ServiceKey, compose};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService, SERVICE_CREDENTIALS,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpService, HttpSseRequest, HttpTransport, SERVICE_HTTP,
    SseEventStream, TransportError,
};
use heycode_llm::{
    AuthenticationBinding, CallPurpose, CapabilitySupport, CatalogRefreshMode, CatalogRegistry,
    ChatMessage, InferenceInput, InferenceTarget, InputModality, LlmSelection, ModelDescriptor,
    ProviderErrorClass, ProviderOptionContext, ProviderRegistry, ReasoningEffortId, RequestDraft,
    SERVICE_MODELS, SERVICE_PROVIDERS, llm_plugin, model_catalog_plugin,
};
use heycode_provider_google::{
    CLAUDE_VERTEX_DEFAULT_MODEL, CLAUDE_VERTEX_PROVIDER, ClaudeVertexControls, ClaudeVertexEffort,
    ClaudeVertexProfile, ClaudeVertexThinking, CodeExecutionRequest, ExternalApiAuth,
    ExternalGroundingRequest, GOOGLE_CLAUDE_VERTEX_MODEL_SOURCE,
    GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE, GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
    GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION, GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
    GOOGLE_GEMINI_3_7_FLASH, GOOGLE_SEARCH_IMPLEMENTATION, GOOGLE_VERTEX_MODEL_SOURCE,
    GOOGLE_VERTEX_PROVIDER, GOOGLE_WEB_SEARCH_LOGICAL, GeminiCacheRequest,
    GoogleGeminiModelEvidence, GoogleGeminiPolicy, GoogleInferencePluginConfig,
    GoogleInferencePluginError, GoogleSearchRequest, MaintainedVertexCatalog,
    google_inference_plugin, maintained_claude_vertex_catalog_plugin,
    maintained_vertex_gemini_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

const VERTEX_MODEL: &str = "gemini-3.7-flash";

struct TestHttpPlugin(HttpService);

impl Plugin for TestHttpPlugin {
    fn name(&self) -> &'static str {
        "test-google-inference-http"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), self.0.clone())
    }
}

struct TestCredentialsPlugin;

impl Plugin for TestCredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-google-inference-credentials"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

struct TestGcpPlugin(GcpAuthService);

impl Plugin for TestGcpPlugin {
    fn name(&self) -> &'static str {
        "test-google-inference-gcp"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_GCP_AUTH]
    }

    fn inject(&self) -> &'static [ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_GCP_AUTH, self.name(), self.0.clone())
    }
}

fn base_plugins() -> Vec<Box<dyn Plugin>> {
    let (http, _) = super::support::http(Vec::new());
    vec![
        Box::new(TestHttpPlugin(http)),
        Box::new(TestCredentialsPlugin),
        heycode_native_tools::native_tools_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "google".to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
    ]
}

fn api_key_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("GEMINI_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn oauth_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE).unwrap(),
        CredentialKind::new("oauth-token").unwrap(),
    )
}

fn explicit_gcp_request(metadata: GcpMetadataPolicy) -> GcpProfileRequest {
    GcpProfileRequest {
        project: Some("vertex-fixture".to_owned()),
        location: Some("global".to_owned()),
        platform: GcpHostPlatform::Unix,
        metadata,
    }
}

fn configured_gcp(http: HttpService) -> GcpAuthService {
    let adc_path = "/fixture/application_default_credentials.json";
    GcpAuthService::new(
        Arc::new(
            MapGcpEnvironment::new()
                .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, adc_path)
                .with_file(adc_path, br#"{"type":"authorized_user"}"#.to_vec()),
        ),
        http,
    )
}

fn request_draft(provider: &str, model: &str, reasoning_effort: Option<&str>) -> RequestDraft {
    RequestDraft {
        provider: provider.to_owned(),
        model: model.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1),
        effective_at_ms: 2,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: reasoning_effort.map(|value| ReasoningEffortId::new(value).unwrap()),
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Conversation,
    }
}

fn model(id: &str) -> ModelDescriptor {
    let mut model = ModelDescriptor::unknown(id);
    model.capabilities.native_web = CapabilitySupport::Supported;
    model.capabilities.prompt_cache = CapabilitySupport::Supported;
    model
}

fn evidence(id: &str) -> GoogleGeminiModelEvidence {
    GoogleGeminiModelEvidence::new(id, vec![model(id)]).unwrap()
}

async fn claude_profile() -> ClaudeVertexProfile {
    let adc_path = "/fixture/application_default_credentials.json";
    let environment = MapGcpEnvironment::new()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, adc_path)
        .with_file(adc_path, br#"{"type":"authorized_user"}"#.to_vec());
    let auth = GcpAuthService::new(Arc::new(environment), super::support::http(Vec::new()).0)
        .resolve(
            GcpProfileRequest {
                project: Some("vertex-fixture".to_owned()),
                location: Some("global".to_owned()),
                platform: GcpHostPlatform::Unix,
                metadata: GcpMetadataPolicy::Disabled,
            },
            CancellationToken::new(),
        )
        .await;
    ClaudeVertexProfile::from_gcp(&auth, oauth_query()).unwrap()
}

#[test]
fn developer_and_vertex_plugins_publish_only_explicit_candidates_and_dispose_them() {
    let developer_policy = GoogleGeminiPolicy::none()
        .with_google_search(
            GoogleSearchRequest::web(),
            vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
        )
        .unwrap()
        .with_code_execution(
            CodeExecutionRequest::new(),
            vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
        )
        .unwrap();
    let developer = GoogleInferencePluginConfig::developer(
        api_key_query(),
        evidence(GOOGLE_GEMINI_3_7_FLASH),
        developer_policy,
    )
    .unwrap();
    let external = ExternalGroundingRequest::simple_search(
        "https://search.example.test/query",
        ExternalApiAuth::no_auth(),
    )
    .unwrap();
    let vertex_policy = GoogleGeminiPolicy::none()
        .with_external_grounding(external, vec![VERTEX_MODEL.to_owned()])
        .unwrap()
        .with_cache(
            GeminiCacheRequest::vertex_explicit(
                "projects/vertex-fixture/locations/global/cachedContents/123456",
            )
            .unwrap(),
        )
        .unwrap();
    let vertex = GoogleInferencePluginConfig::vertex(
        "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/google",
        oauth_query(),
        evidence(VERTEX_MODEL),
        vertex_policy,
    )
    .unwrap();
    let mut plugins = base_plugins();
    plugins.push(google_inference_plugin(developer));
    plugins.push(google_inference_plugin(vertex));
    let mut context = compose(&plugins).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let native = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    assert_eq!(providers.names(), vec!["google", GOOGLE_VERTEX_PROVIDER]);
    let google_routes = native.resolve("google").unwrap();
    assert_eq!(google_routes.len(), 2);
    assert!(google_routes.iter().any(|route| {
        route.logical() == GOOGLE_WEB_SEARCH_LOGICAL
            && route.implementation() == GOOGLE_SEARCH_IMPLEMENTATION
    }));
    assert!(google_routes.iter().any(|route| {
        route.logical() == "code_execution"
            && route.implementation() == GOOGLE_CODE_EXECUTION_IMPLEMENTATION
    }));
    let vertex_routes = native.resolve(GOOGLE_VERTEX_PROVIDER).unwrap();
    assert_eq!(vertex_routes.len(), 1);
    assert_eq!(
        vertex_routes[0].logical(),
        GOOGLE_EXTERNAL_GROUNDING_LOGICAL
    );
    assert_eq!(
        vertex_routes[0].implementation(),
        "vertex-google:external_grounding"
    );
    assert_eq!(
        GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
        "vertex-google:external_grounding"
    );
    let vertex_provider = providers.get(GOOGLE_VERTEX_PROVIDER).unwrap();
    let selected = model(VERTEX_MODEL);
    let options = vertex_provider
        .request_options_for(ProviderOptionContext::new(&selected, &vertex_routes))
        .unwrap();
    assert_eq!(options.len(), 2);
    assert!(
        options
            .iter()
            .all(|option| option.provider() == GOOGLE_VERTEX_PROVIDER)
    );
    let snapshot = context.plugin_inventory().snapshot().unwrap();
    assert!(snapshot.contributions.iter().any(|row| {
        row.plugin == "inference-google-vertex"
            && row.kind == ContributionKind::NativeTool
            && row.name == GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION
    }));

    context.shutdown();
    assert!(providers.names().is_empty());
    assert!(native.resolve("google").unwrap().is_empty());
    assert!(native.resolve(GOOGLE_VERTEX_PROVIDER).unwrap().is_empty());
}

#[tokio::test]
async fn exact_model_claude_vertex_plugin_is_effect_owned() {
    let profile = claude_profile().await;
    let expected_model = profile.model().clone();
    let config = GoogleInferencePluginConfig::claude_vertex(
        profile,
        ClaudeVertexControls::new(ClaudeVertexThinking::Adaptive, ClaudeVertexEffort::XHigh),
    )
    .unwrap();
    let mut plugins = base_plugins();
    plugins.push(google_inference_plugin(config));
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let provider = registry.get(CLAUDE_VERTEX_PROVIDER).unwrap();
    assert_eq!(provider.info().default_model, CLAUDE_VERTEX_DEFAULT_MODEL);
    assert_eq!(
        provider.descriptor().protocols,
        [heycode_core::ProviderProtocol::AnthropicMessages]
    );
    assert_eq!(
        provider.credential_reference(),
        Some("gcp:cloud-platform-access-token")
    );
    assert_eq!(
        provider.describe_model(CLAUDE_VERTEX_DEFAULT_MODEL),
        expected_model
    );
    assert_eq!(
        provider.describe_model("claude-unknown").capabilities.tools,
        CapabilitySupport::Unknown
    );
    context.shutdown();
    assert!(registry.get(CLAUDE_VERTEX_PROVIDER).is_none());
}

#[test]
fn product_configs_reject_cross_product_policy_and_unevidenced_models() {
    let external = ExternalGroundingRequest::simple_search(
        "https://search.example.test/query",
        ExternalApiAuth::no_auth(),
    )
    .unwrap();
    let external_policy = GoogleGeminiPolicy::none()
        .with_external_grounding(external, vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()])
        .unwrap();
    assert!(
        GoogleInferencePluginConfig::developer(
            api_key_query(),
            evidence(GOOGLE_GEMINI_3_7_FLASH),
            external_policy,
        )
        .is_err()
    );
    let search_policy = GoogleGeminiPolicy::none()
        .with_google_search(GoogleSearchRequest::web(), vec!["unproven".to_owned()])
        .unwrap();
    assert!(
        GoogleInferencePluginConfig::developer(
            api_key_query(),
            evidence(GOOGLE_GEMINI_3_7_FLASH),
            search_policy,
        )
        .is_err()
    );

    let vertex_cache = GoogleGeminiPolicy::none()
        .with_cache(
            GeminiCacheRequest::vertex_explicit(
                "projects/vertex-fixture/locations/global/cachedContents/123456",
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        GoogleInferencePluginConfig::developer(
            api_key_query(),
            evidence(GOOGLE_GEMINI_3_7_FLASH),
            vertex_cache,
        )
        .err()
        .expect("Vertex cache must be refused by the Developer product"),
        GoogleInferencePluginError::ProductPolicyMismatch
    );

    let developer_cache = GoogleGeminiPolicy::none()
        .with_cache(GeminiCacheRequest::explicit("cachedContents/cache-1").unwrap())
        .unwrap();
    assert_eq!(
        GoogleInferencePluginConfig::vertex(
            "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/google",
            oauth_query(),
            evidence(VERTEX_MODEL),
            developer_cache,
        )
        .err()
        .expect("Developer cache must be refused by the Vertex product"),
        GoogleInferencePluginError::ProductPolicyMismatch
    );

    assert_eq!(
        GoogleInferencePluginConfig::lazy_vertex(
            GcpProfileRequest::default(),
            oauth_query(),
            GoogleGeminiPolicy::none(),
        )
        .err()
        .expect("lazy Vertex route must not inherit project/location defaults"),
        GoogleInferencePluginError::ProfileRouteInvalid
    );

    let mut regional_claude = explicit_gcp_request(GcpMetadataPolicy::Disabled);
    regional_claude.location = Some("us-east5".to_owned());
    assert_eq!(
        GoogleInferencePluginConfig::lazy_claude_vertex(
            regional_claude,
            oauth_query(),
            ClaudeVertexControls::sonnet_five_default(),
        )
        .err()
        .expect("Sonnet 5 lazy activation must reject unsupported regional routing"),
        GoogleInferencePluginError::ProfileRouteInvalid
    );
}

#[test]
fn explicit_empty_policy_registers_no_native_candidate() {
    let config = GoogleInferencePluginConfig::developer(
        api_key_query(),
        evidence(GOOGLE_GEMINI_3_7_FLASH),
        GoogleGeminiPolicy::none(),
    )
    .unwrap();
    let mut plugins = base_plugins();
    plugins.push(google_inference_plugin(config));
    let mut context = compose(&plugins).unwrap();
    let native = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    assert!(native.resolve("google").unwrap().is_empty());
    let snapshot = context.plugin_inventory().snapshot().unwrap();
    assert!(!snapshot.contributions.iter().any(|row| {
        row.plugin == "inference-google-gemini" && row.kind == ContributionKind::NativeTool
    }));
    context.shutdown();
    assert!(native.resolve("google").unwrap().is_empty());
}

#[tokio::test]
async fn maintained_catalogs_match_their_inference_routes_without_claiming_account_access() {
    let vertex_source = MaintainedVertexCatalog::vertex_gemini();
    let claude_source = MaintainedVertexCatalog::claude_vertex();
    assert_eq!(vertex_source.account_access(), GcpHealth::Unknown);
    assert_eq!(claude_source.account_access(), GcpHealth::Unknown);
    assert_eq!(vertex_source.source_url(), GOOGLE_VERTEX_MODEL_SOURCE);
    assert_eq!(
        claude_source.source_url(),
        GOOGLE_CLAUDE_VERTEX_MODEL_SOURCE
    );

    let vertex_evidence = GoogleGeminiModelEvidence::maintained_vertex();
    let vertex_config = GoogleInferencePluginConfig::vertex(
        "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/google",
        oauth_query(),
        vertex_evidence,
        GoogleGeminiPolicy::none(),
    )
    .unwrap();
    let claude_config = GoogleInferencePluginConfig::claude_vertex(
        claude_profile().await,
        ClaudeVertexControls::sonnet_five_default(),
    )
    .unwrap();
    let (http, requests) = super::support::http(Vec::new());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(TestHttpPlugin(http)),
        Box::new(TestCredentialsPlugin),
        heycode_native_tools::native_tools_plugin(),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: GOOGLE_VERTEX_PROVIDER.to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
        maintained_vertex_gemini_catalog_plugin(),
        maintained_claude_vertex_catalog_plugin(),
        google_inference_plugin(vertex_config),
        google_inference_plugin(claude_config),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();

    for (provider_id, model_id) in [
        (GOOGLE_VERTEX_PROVIDER, GOOGLE_GEMINI_3_7_FLASH),
        (CLAUDE_VERTEX_PROVIDER, CLAUDE_VERTEX_DEFAULT_MODEL),
    ] {
        let view = models
            .refresh(
                provider_id,
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(view.snapshot.provider.id, provider_id);
        assert_eq!(view.snapshot.models.len(), 1);
        let catalog_model = &view.snapshot.models[0];
        assert_eq!(catalog_model.id, model_id);
        assert_eq!(
            catalog_model.capabilities.native_compaction,
            CapabilitySupport::Unknown
        );
        if provider_id == GOOGLE_VERTEX_PROVIDER {
            assert_eq!(
                catalog_model.capabilities.document_input,
                CapabilitySupport::Unknown
            );
        } else {
            assert_eq!(
                catalog_model.capabilities.native_web,
                CapabilitySupport::Unknown
            );
        }
        assert_eq!(
            providers.get(provider_id).unwrap().describe_model(model_id),
            *catalog_model
        );
    }
    assert!(requests.lock().unwrap().is_empty());
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for (plugin, provider) in [
        ("catalog-google-vertex", GOOGLE_VERTEX_PROVIDER),
        ("catalog-google-claude-vertex", CLAUDE_VERTEX_PROVIDER),
    ] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == plugin
                && row.kind == ContributionKind::ModelCatalog
                && row.name == provider
        }));
    }

    context.shutdown();
    assert!(models.descriptors().unwrap().is_empty());
    assert!(providers.names().is_empty());
}

#[tokio::test]
async fn lazy_vertex_plugins_prepare_exact_targets_without_composition_io_and_withdraw_lifo() {
    let (http, requests) = super::support::http(Vec::new());
    let vertex = GoogleInferencePluginConfig::lazy_vertex(
        explicit_gcp_request(GcpMetadataPolicy::Disabled),
        oauth_query(),
        GoogleGeminiPolicy::none(),
    )
    .unwrap();
    let claude = GoogleInferencePluginConfig::lazy_claude_vertex(
        explicit_gcp_request(GcpMetadataPolicy::Disabled),
        oauth_query(),
        ClaudeVertexControls::sonnet_five_default(),
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(TestHttpPlugin(http.clone())),
        Box::new(TestCredentialsPlugin),
        Box::new(TestGcpPlugin(configured_gcp(http))),
        heycode_native_tools::native_tools_plugin(),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: GOOGLE_VERTEX_PROVIDER.to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
        google_inference_plugin(vertex),
        google_inference_plugin(claude),
    ];
    let mut context = compose(&plugins).unwrap();
    assert!(requests.lock().unwrap().is_empty());
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();

    for (provider_id, model_id, expected_target, effort) in [
        (
            GOOGLE_VERTEX_PROVIDER,
            GOOGLE_GEMINI_3_7_FLASH,
            "https://aiplatform.googleapis.com/v1/projects/vertex-fixture/locations/global/publishers/google",
            None,
        ),
        (
            CLAUDE_VERTEX_PROVIDER,
            CLAUDE_VERTEX_DEFAULT_MODEL,
            "https://aiplatform.googleapis.com/v1/projects/vertex-fixture/locations/global/publishers/anthropic/models/claude-sonnet-5:streamRawPredict",
            Some("high"),
        ),
    ] {
        let catalog = models
            .refresh(
                provider_id,
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let model = catalog.snapshot.models[0].clone();
        let inert = providers.get(provider_id).unwrap();
        assert!(inert.inference_adapter().is_some());
        let prepared = inert
            .prepare_inference(
                ProviderOptionContext::new(&model, &[]),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .expect("lazy provider must return one operation provider");
        assert_eq!(prepared.describe_model(model_id), model);
        let adapter = prepared.inference_adapter().unwrap();
        let call = adapter
            .resolve(request_draft(provider_id, model_id, effort), &model)
            .unwrap();
        assert_eq!(
            call.target(),
            &InferenceTarget::Http {
                base_url: expected_target.to_owned()
            }
        );
        assert!(matches!(
            call.authentication(),
            AuthenticationBinding::Credential(handle)
                if handle.as_str() == GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE
        ));
    }
    assert!(requests.lock().unwrap().is_empty());
    let inventory = context.plugin_inventory().snapshot().unwrap();
    for (plugin, provider_id) in [
        ("inference-google-vertex", GOOGLE_VERTEX_PROVIDER),
        ("inference-google-claude-vertex", CLAUDE_VERTEX_PROVIDER),
    ] {
        for kind in [
            ContributionKind::ModelCatalog,
            ContributionKind::InferenceProvider,
        ] {
            assert!(inventory.contributions.iter().any(|row| {
                row.plugin == plugin && row.kind == kind && row.name == provider_id
            }));
        }
    }

    context.shutdown();
    assert!(providers.names().is_empty());
    assert!(models.descriptors().unwrap().is_empty());
}

struct CancelledMetadataTransport {
    started: Arc<tokio::sync::Notify>,
    settled: Arc<AtomicUsize>,
}

impl HttpTransport for CancelledMetadataTransport {
    fn send(
        &self,
        _request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let started = self.started.clone();
        let settled = self.settled.clone();
        Box::pin(async move {
            started.notify_one();
            cancellation.cancelled().await;
            settled.fetch_add(1, Ordering::SeqCst);
            Err(TransportError::Cancelled)
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

#[tokio::test]
async fn lazy_vertex_preparation_cancellation_settles_the_gcp_profile_probe() {
    let started = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(AtomicUsize::new(0));
    let http = HttpService::new(Arc::new(CancelledMetadataTransport {
        started: started.clone(),
        settled: settled.clone(),
    }));
    let config = GoogleInferencePluginConfig::lazy_vertex(
        explicit_gcp_request(GcpMetadataPolicy::Probe {
            budget: Duration::from_secs(60),
        }),
        oauth_query(),
        GoogleGeminiPolicy::none(),
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(TestHttpPlugin(http.clone())),
        Box::new(TestCredentialsPlugin),
        Box::new(TestGcpPlugin(GcpAuthService::new(
            Arc::new(MapGcpEnvironment::new()),
            http,
        ))),
        heycode_native_tools::native_tools_plugin(),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: GOOGLE_VERTEX_PROVIDER.to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
        google_inference_plugin(config),
    ];
    let mut context = compose(&plugins).unwrap();
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let provider = providers.get(GOOGLE_VERTEX_PROVIDER).unwrap();
    let cancellation = CancellationToken::new();
    let operation = {
        let provider = provider.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            let model = provider.describe_model(GOOGLE_GEMINI_3_7_FLASH);
            provider
                .prepare_inference(ProviderOptionContext::new(&model, &[]), cancellation)
                .await
        })
    };
    started.notified().await;
    cancellation.cancel();
    let error = operation
        .await
        .unwrap()
        .err()
        .expect("cancelled preparation must fail");
    assert_eq!(
        error.provider_failure().map(|failure| failure.class()),
        Some(ProviderErrorClass::Cancelled)
    );
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    context.shutdown();
}

#[tokio::test]
async fn lazy_vertex_preparation_refuses_catalog_and_native_route_drift() {
    let (http, requests) = super::support::http(Vec::new());
    let config = GoogleInferencePluginConfig::lazy_vertex(
        explicit_gcp_request(GcpMetadataPolicy::Disabled),
        oauth_query(),
        GoogleGeminiPolicy::none(),
    )
    .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(TestHttpPlugin(http.clone())),
        Box::new(TestCredentialsPlugin),
        Box::new(TestGcpPlugin(configured_gcp(http))),
        heycode_native_tools::native_tools_plugin(),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: GOOGLE_VERTEX_PROVIDER.to_owned(),
                model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
            },
            Vec::new(),
        ),
        google_inference_plugin(config),
    ];
    let mut context = compose(&plugins).unwrap();
    let provider = context
        .get::<ProviderRegistry>(SERVICE_PROVIDERS)
        .unwrap()
        .get(GOOGLE_VERTEX_PROVIDER)
        .unwrap();
    let mut drifted_model = provider.describe_model(GOOGLE_GEMINI_3_7_FLASH);
    drifted_model.context_window = Some(1);
    let model_error = provider
        .prepare_inference(
            ProviderOptionContext::new(&drifted_model, &[]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("catalog drift must fail preparation");
    assert_eq!(
        model_error
            .provider_failure()
            .map(|failure| failure.class()),
        Some(ProviderErrorClass::InvalidRequest)
    );

    let model = provider.describe_model(GOOGLE_GEMINI_3_7_FLASH);
    let drifted_route = heycode_core::NativeToolRoute::new(
        GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
        "vertex-google:unconfigured",
        heycode_core::NativeToolImplementationKind::Provider,
        Some(GOOGLE_VERTEX_PROVIDER.to_owned()),
    )
    .unwrap();
    let route_error = provider
        .prepare_inference(
            ProviderOptionContext::new(&model, &[drifted_route]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("native-route drift must fail preparation");
    assert_eq!(
        route_error
            .provider_failure()
            .map(|failure| failure.class()),
        Some(ProviderErrorClass::InvalidRequest)
    );
    assert!(requests.lock().unwrap().is_empty());
    context.shutdown();
}

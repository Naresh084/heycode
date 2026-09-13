//! P09 per-operation credential resolution.
//!
//! Two properties are pinned here. **Rotation reaches the next request**: a
//! key changed in the backing store is sent by the next operation on the same
//! adapter instance, with no recomposition. **No cross-route fallback**: a
//! route with no credential fails without dispatching, rather than going out
//! authenticated as a route that does have one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialResolutionError, CredentialSecret,
    CredentialSource, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpSseRequest, HttpTransport, SseEvent, SseEventStream,
    TransportError,
};
use heycode_llm::{
    AnthropicMessagesAdapter, AnthropicMessagesConfig, AuthenticationBinding,
    BedrockConverseAdapter, BedrockConverseConfig, CallPurpose, CapabilitySupport, ChatMessage,
    CredentialHandle, CredentialResolver, GeminiAdapter, GeminiConfig, InferenceAdapter,
    InferenceInput, InputModality, LlmError, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ModelPerformance, ModelPricing, OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig,
    OpenAiResponsesAdapter, OpenAiResponsesConfig, ProviderDescriptor, ProviderErrorClass,
    ProviderProtocol, RequestDraft, RetrySpec, RouteCredential,
};
use tokio_util::sync::CancellationToken;

const OPENAI_ROUTE: &str = "OPENAI_API_KEY";
const ANTHROPIC_ROUTE: &str = "ANTHROPIC_API_KEY";

// ---------------------------------------------------------------- fixtures

/// Records every request it is handed and answers with a transport failure.
/// The request is fully built — including its authorization header — before
/// the transport is reached, so the recording is the wire evidence.
struct RecordingTransport {
    requests: Mutex<Vec<Vec<(String, String)>>>,
    scripted: Mutex<Vec<Vec<Result<SseEvent, TransportError>>>>,
}

impl RecordingTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            scripted: Mutex::new(Vec::new()),
        })
    }

    /// Scripted attempt outcomes, consumed front to back.
    fn scripted(attempts: Vec<Vec<Result<SseEvent, TransportError>>>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            scripted: Mutex::new(attempts),
        })
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn header(&self, index: usize, name: &str) -> Option<String> {
        self.requests
            .lock()
            .unwrap()
            .get(index)
            .and_then(|headers| {
                headers
                    .iter()
                    .find(|(header, _)| header.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.clone())
            })
    }

    fn record(&self, headers: Vec<(String, String)>) {
        self.requests.lock().unwrap().push(headers);
    }

    fn next_attempt(&self) -> Vec<Result<SseEvent, TransportError>> {
        let mut scripted = self.scripted.lock().unwrap();
        if scripted.is_empty() {
            vec![Err(TransportError::Network {
                message: "recorded".to_owned(),
            })]
        } else {
            scripted.remove(0)
        }
    }
}

impl HttpTransport for RecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.record(
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        );
        Box::pin(futures::stream::iter(self.next_attempt()))
    }

    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.record(
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        );
        Box::pin(async {
            Err(TransportError::Network {
                message: "recorded".to_owned(),
            })
        })
    }
}

/// A keychain-shaped store whose records can be rotated between operations,
/// plus a count of the resolutions it actually served.
struct RotatingStore {
    id: CredentialProviderId,
    records: Mutex<BTreeMap<String, String>>,
    resolutions: AtomicUsize,
    references_seen: Mutex<Vec<String>>,
}

impl RotatingStore {
    fn new(records: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            id: CredentialProviderId::new("keychain").unwrap(),
            records: Mutex::new(
                records
                    .iter()
                    .map(|(reference, secret)| ((*reference).to_owned(), (*secret).to_owned()))
                    .collect(),
            ),
            resolutions: AtomicUsize::new(0),
            references_seen: Mutex::new(Vec::new()),
        })
    }

    fn rotate(&self, reference: &str, secret: &str) {
        self.records
            .lock()
            .unwrap()
            .insert(reference.to_owned(), secret.to_owned());
    }

    fn resolutions(&self) -> usize {
        self.resolutions.load(Ordering::SeqCst)
    }

    fn references_seen(&self) -> Vec<String> {
        let mut seen = self.references_seen.lock().unwrap().clone();
        seen.sort();
        seen.dedup();
        seen
    }
}

impl CredentialProvider for RotatingStore {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        10
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        self.references_seen
            .lock()
            .unwrap()
            .push(query.reference.as_str().to_owned());
        Ok(
            if self
                .records
                .lock()
                .unwrap()
                .contains_key(query.reference.as_str())
            {
                CredentialProviderState::configured(CredentialSource::Keychain, true)
            } else {
                CredentialProviderState::unconfigured(true)
            },
        )
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        self.references_seen
            .lock()
            .unwrap()
            .push(query.reference.as_str().to_owned());
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .records
            .lock()
            .unwrap()
            .get(query.reference.as_str())
            .map(CredentialSecret::new))
    }
}

/// A resolver bound to one route that counts how often it is consulted.
struct CountingResolver {
    route: CredentialHandle,
    secret: Mutex<String>,
    calls: AtomicUsize,
}

impl CountingResolver {
    fn new(route: &str, secret: &str) -> Arc<Self> {
        Arc::new(Self {
            route: CredentialHandle::new(route).unwrap(),
            secret: Mutex::new(secret.to_owned()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl CredentialResolver for CountingResolver {
    fn route(&self) -> &CredentialHandle {
        &self.route
    }

    fn resolve(
        &self,
        _route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CredentialSecret::new(self.secret.lock().unwrap().clone()))
    }
}

fn query(reference: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(reference).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn registry(store: Arc<RotatingStore>) -> (heycode_core::Context, CredentialsService) {
    let service = CredentialsService::new();
    let context = heycode_core::Context::new();
    service.register(&context, store).unwrap();
    (context, service)
}

fn provider(protocol: ProviderProtocol) -> ProviderDescriptor {
    ProviderDescriptor {
        id: "route".to_owned(),
        display_name: "Route".to_owned(),
        protocols: vec![protocol],
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: "model".to_owned(),
        display_name: "Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(8_192),
        max_output_tokens: Some(2_048),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Unsupported,
            reasoning: CapabilitySupport::Unsupported,
            image_input: CapabilitySupport::Unsupported,
            document_input: CapabilitySupport::Unsupported,
            structured_output: CapabilitySupport::Unsupported,
            native_web: CapabilitySupport::Unsupported,
            native_compaction: CapabilitySupport::Unsupported,
            prompt_cache: CapabilitySupport::Unsupported,
        },
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "route".to_owned(),
        model: "model".to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Evaluation,
    }
}

fn chat_adapter(
    credential: RouteCredential,
    transport: Arc<RecordingTransport>,
) -> OpenAiChatCompletionsAdapter {
    OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_credential(
            provider(ProviderProtocol::OpenAiChatCompletions),
            "https://route.test/v1",
            credential,
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(transport),
    )
    .unwrap()
}

/// Run one complete operation and discard its events; the wire evidence is
/// what the transport recorded.
async fn run(adapter: &dyn InferenceAdapter) {
    let call = adapter.resolve(draft(), &model()).unwrap();
    let _events = adapter.stream(call).collect::<Vec<_>>().await;
}

// ------------------------------------------- rotated key reaches next request

#[tokio::test]
async fn a_key_rotated_in_the_store_is_sent_by_the_next_request_on_the_same_adapter() {
    let store = RotatingStore::new(&[(OPENAI_ROUTE, "sk-first")]);
    let (mut context, credentials) = registry(store.clone());
    let transport = RecordingTransport::new();
    let adapter = chat_adapter(
        RouteCredential::registry(credentials, query(OPENAI_ROUTE)),
        transport.clone(),
    );

    run(&adapter).await;
    assert_eq!(
        transport.header(0, "authorization").as_deref(),
        Some("Bearer sk-first")
    );

    store.rotate(OPENAI_ROUTE, "sk-rotated");
    run(&adapter).await;
    assert_eq!(
        transport.header(1, "authorization").as_deref(),
        Some("Bearer sk-rotated"),
        "the adapter was never rebuilt: a key captured at composition would still be sent"
    );
    assert_eq!(store.resolutions(), 2, "one resolution per operation");
    context.shutdown();
}

#[tokio::test]
async fn a_route_configured_after_composition_authenticates_the_next_request() {
    let store = RotatingStore::new(&[]);
    let (mut context, credentials) = registry(store.clone());
    let transport = RecordingTransport::new();
    // Construction must succeed with nothing in the store: for a per-operation
    // route, presence is an operation-time fact, not a construction-time one.
    let adapter = chat_adapter(
        RouteCredential::registry(credentials, query(OPENAI_ROUTE)),
        transport.clone(),
    );

    run(&adapter).await;
    assert_eq!(
        transport.calls(),
        0,
        "an unresolvable route must not dispatch"
    );

    store.rotate(OPENAI_ROUTE, "sk-connected");
    run(&adapter).await;
    assert_eq!(
        transport.header(0, "authorization").as_deref(),
        Some("Bearer sk-connected")
    );
    context.shutdown();
}

#[tokio::test]
async fn every_protocol_adapter_sends_the_key_resolved_for_the_current_operation() {
    let resolver = CountingResolver::new(OPENAI_ROUTE, "sk-first");
    let credential = RouteCredential::per_operation(
        CredentialHandle::new(OPENAI_ROUTE).unwrap(),
        resolver.clone(),
    );

    // (adapter label, auth header, adapter)
    let transports: Vec<Arc<RecordingTransport>> =
        (0..5).map(|_| RecordingTransport::new()).collect();
    let chat = chat_adapter(credential.clone(), transports[0].clone());
    let responses = OpenAiResponsesAdapter::new(
        OpenAiResponsesConfig::with_credential(
            provider(ProviderProtocol::OpenAiResponses),
            "https://route.test/v1",
            credential.clone(),
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(transports[1].clone()),
    )
    .unwrap();
    let anthropic = AnthropicMessagesAdapter::new(
        AnthropicMessagesConfig::with_credential(
            provider(ProviderProtocol::AnthropicMessages),
            "https://route.test/v1",
            credential.clone(),
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(transports[2].clone()),
    )
    .unwrap();
    let gemini = GeminiAdapter::new(
        GeminiConfig::with_credential(
            provider(ProviderProtocol::GeminiGenerateContent),
            "https://route.test/v1",
            credential.clone(),
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(transports[3].clone()),
    )
    .unwrap();
    let bedrock = BedrockConverseAdapter::new(
        BedrockConverseConfig::with_credential(
            provider(ProviderProtocol::BedrockConverse),
            "https://route.test",
            credential,
        )
        .with_retry_spec(RetrySpec::no_retry()),
        heycode_http::HttpService::new(transports[4].clone()),
    )
    .unwrap();

    let adapters: [(&str, &str, &dyn InferenceAdapter); 5] = [
        ("chat", "authorization", &chat),
        ("responses", "authorization", &responses),
        ("anthropic", "x-api-key", &anthropic),
        ("gemini", "x-goog-api-key", &gemini),
        ("bedrock", "authorization", &bedrock),
    ];
    for (index, (label, header, adapter)) in adapters.into_iter().enumerate() {
        run(adapter).await;
        *resolver.secret.lock().unwrap() = "sk-rotated".to_owned();
        run(adapter).await;
        let transport = &transports[index];
        let sent: Vec<String> = (0..2)
            .filter_map(|attempt| transport.header(attempt, header))
            .collect();
        let expected = if header == "authorization" {
            vec!["Bearer sk-first".to_owned(), "Bearer sk-rotated".to_owned()]
        } else {
            vec!["sk-first".to_owned(), "sk-rotated".to_owned()]
        };
        assert_eq!(sent, expected, "{label} must resolve per operation");
        *resolver.secret.lock().unwrap() = "sk-first".to_owned();
    }
}

#[tokio::test]
async fn one_operation_resolves_once_and_reuses_that_key_across_its_retry_attempts() {
    let resolver = CountingResolver::new(OPENAI_ROUTE, "sk-retry");
    let transport = RecordingTransport::scripted(vec![
        vec![Err(TransportError::http(
            503,
            "overloaded",
            heycode_http::HttpErrorMetadata::new(None, None),
        ))],
        vec![Ok(SseEvent {
            event: "message".to_owned(),
            data: serde_json::json!({
                "id":"retry",
                "choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1}
            })
            .to_string(),
            id: None,
            retry_ms: None,
        })],
    ]);
    let adapter = OpenAiChatCompletionsAdapter::new(
        OpenAiChatCompletionsConfig::with_credential(
            provider(ProviderProtocol::OpenAiChatCompletions),
            "https://route.test/v1",
            RouteCredential::per_operation(
                CredentialHandle::new(OPENAI_ROUTE).unwrap(),
                resolver.clone(),
            ),
        )
        .with_retry_spec(
            RetrySpec::new(
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(2),
                std::time::Duration::from_secs(10),
                heycode_llm::RetryJitter::None,
                heycode_llm::RetrySafety::StatelessPreOutput,
            )
            .unwrap(),
        ),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();

    run(&adapter).await;
    assert_eq!(transport.calls(), 2, "the operation retried once");
    assert_eq!(
        resolver.calls(),
        1,
        "a retry storm must not become a keychain-prompt storm"
    );
    assert_eq!(
        transport.header(1, "authorization").as_deref(),
        Some("Bearer sk-retry")
    );
}

#[tokio::test]
async fn the_legacy_openai_compatible_route_also_resolves_per_operation() {
    let resolver = CountingResolver::new(OPENAI_ROUTE, "sk-legacy-first");
    let transport = RecordingTransport::new();
    let provider = heycode_llm::DeepSeekProvider::from_credential_with_transport(
        RouteCredential::per_operation(
            CredentialHandle::new(OPENAI_ROUTE).unwrap(),
            resolver.clone(),
        ),
        None,
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    let request = heycode_llm::ChatRequest {
        model: "deepseek-chat".to_owned(),
        messages: vec![ChatMessage::user("hello")],
        tools: None,
        temperature: None,
        max_tokens: None,
    };

    let _first = heycode_llm::Provider::stream(&provider, request.clone())
        .collect::<Vec<_>>()
        .await;
    *resolver.secret.lock().unwrap() = "sk-legacy-rotated".to_owned();
    let _second = heycode_llm::Provider::stream(&provider, request)
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        transport.header(0, "authorization").as_deref(),
        Some("Bearer sk-legacy-first")
    );
    assert_eq!(
        transport.header(1, "authorization").as_deref(),
        Some("Bearer sk-legacy-rotated"),
        "the legacy compatibility route must resolve per operation too"
    );
    assert_eq!(resolver.calls(), 2);
}

// ------------------------------------------------- no cross-route fallback

#[tokio::test]
async fn a_route_with_no_credential_dispatches_nothing_rather_than_another_routes_key() {
    let store = RotatingStore::new(&[(ANTHROPIC_ROUTE, "sk-ant-neighbour")]);
    let (mut context, credentials) = registry(store.clone());
    let transport = RecordingTransport::new();
    let adapter = chat_adapter(
        RouteCredential::registry(credentials, query(OPENAI_ROUTE)),
        transport.clone(),
    );

    let call = adapter.resolve(draft(), &model()).unwrap();
    let events = adapter.stream(call).collect::<Vec<_>>().await;

    assert_eq!(
        transport.calls(),
        0,
        "a request that cannot authenticate as its own route must never be sent"
    );
    assert_eq!(
        store.references_seen(),
        vec![OPENAI_ROUTE.to_owned()],
        "resolving one route must never consult a second reference"
    );
    let error = match events.as_slice() {
        [Err(error)] => error,
        other => panic!("expected exactly one failure, got {other:?}"),
    };
    assert_eq!(error.class(), ProviderErrorClass::Authentication);
    let rendered = error.to_string();
    assert!(rendered.contains(OPENAI_ROUTE), "{rendered}");
    assert!(
        !rendered.contains(ANTHROPIC_ROUTE),
        "the failure must not disclose which other routes are configured: {rendered}"
    );
    assert!(!rendered.contains("sk-ant-neighbour"), "{rendered}");
    assert!(
        !format!("{error:?}").contains("sk-ant-neighbour"),
        "debug output must not carry another route's secret"
    );
    context.shutdown();
}

#[test]
fn a_resolver_bound_to_another_route_is_refused_before_it_is_consulted() {
    let resolver = CountingResolver::new(ANTHROPIC_ROUTE, "sk-ant-neighbour");
    let credential = RouteCredential::per_operation(
        CredentialHandle::new(OPENAI_ROUTE).unwrap(),
        resolver.clone(),
    );

    let error = credential.acquire().unwrap_err();
    assert_eq!(
        error,
        CredentialResolutionError::RouteMismatch {
            reference: OPENAI_ROUTE.to_owned()
        }
    );
    assert_eq!(
        resolver.calls(),
        0,
        "the mismatch must be refused before the foreign resolver runs"
    );
    let rendered = error.to_string();
    assert!(rendered.contains(OPENAI_ROUTE), "{rendered}");
    assert!(!rendered.contains(ANTHROPIC_ROUTE), "{rendered}");
}

#[test]
fn the_registry_resolver_refuses_a_foreign_route_without_consulting_the_registry() {
    let store = RotatingStore::new(&[(ANTHROPIC_ROUTE, "sk-ant-neighbour")]);
    let (mut context, credentials) = registry(store.clone());
    let credential = RouteCredential::registry(credentials, query(ANTHROPIC_ROUTE));
    let resolver: Arc<dyn CredentialResolver> = credential.resolver().unwrap();

    let error = resolver
        .resolve(&CredentialHandle::new(OPENAI_ROUTE).unwrap())
        .unwrap_err();
    assert_eq!(
        error,
        CredentialResolutionError::RouteMismatch {
            reference: OPENAI_ROUTE.to_owned()
        }
    );
    assert!(
        store.references_seen().is_empty(),
        "a foreign-route call must not reach the credential registry at all"
    );
    context.shutdown();
}

// ------------------------------------------------------- binding honesty

#[test]
fn a_per_operation_route_records_its_reference_while_a_fixed_key_records_adapter_owned() {
    let per_operation = RouteCredential::per_operation(
        CredentialHandle::new(OPENAI_ROUTE).unwrap(),
        CountingResolver::new(OPENAI_ROUTE, "sk-live"),
    );
    let fixed = RouteCredential::fixed("sk-fixed");

    for (credential, expected) in [
        (
            per_operation.clone(),
            AuthenticationBinding::Credential(CredentialHandle::new(OPENAI_ROUTE).unwrap()),
        ),
        (
            fixed.clone(),
            AuthenticationBinding::AdapterOwned(heycode_llm::AdapterOwnedAuth::new()),
        ),
    ] {
        let chat = OpenAiChatCompletionsConfig::with_credential(
            provider(ProviderProtocol::OpenAiChatCompletions),
            "https://route.test/v1",
            credential.clone(),
        );
        assert_eq!(chat.resolve_spec().authentication, expected);
        let responses = OpenAiResponsesConfig::with_credential(
            provider(ProviderProtocol::OpenAiResponses),
            "https://route.test/v1",
            credential.clone(),
        );
        assert_eq!(responses.resolve_spec().authentication, expected);
        let anthropic = AnthropicMessagesConfig::with_credential(
            provider(ProviderProtocol::AnthropicMessages),
            "https://route.test/v1",
            credential.clone(),
        );
        assert_eq!(anthropic.resolve_spec().authentication, expected);
        let gemini = GeminiConfig::with_credential(
            provider(ProviderProtocol::GeminiGenerateContent),
            "https://route.test/v1",
            credential.clone(),
        );
        assert_eq!(gemini.resolve_spec().authentication, expected);
        let bedrock = BedrockConverseConfig::with_credential(
            provider(ProviderProtocol::BedrockConverse),
            "https://route.test",
            credential,
        );
        assert_eq!(bedrock.resolve_spec().authentication, expected);
    }
    assert_eq!(
        per_operation.route().map(CredentialHandle::as_str),
        Some(OPENAI_ROUTE)
    );
    assert_eq!(fixed.route(), None);
}

// ------------------------------------------------------------- redaction

#[test]
fn no_credential_surface_renders_a_secret_value() {
    const SECRET: &str = "sk-must-never-be-printed";
    let fixed = RouteCredential::fixed(SECRET);
    let per_operation = RouteCredential::per_operation(
        CredentialHandle::new(OPENAI_ROUTE).unwrap(),
        CountingResolver::new(OPENAI_ROUTE, SECRET),
    );

    for credential in [&fixed, &per_operation] {
        let rendered = format!("{credential:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        let acquired = credential.acquire().unwrap();
        assert_eq!(acquired.expose(), SECRET, "the seam must still deliver it");
        assert!(!format!("{acquired:?}").contains(SECRET));

        let config = OpenAiChatCompletionsConfig::with_credential(
            provider(ProviderProtocol::OpenAiChatCompletions),
            "https://route.test/v1",
            credential.clone(),
        );
        assert!(!format!("{config:?}").contains(SECRET));
    }
    // The per-operation arm names its route, which is deliberately non-secret.
    assert!(format!("{per_operation:?}").contains(OPENAI_ROUTE));

    let error = LlmError::UnresolvedCredential(CredentialResolutionError::Missing {
        reference: OPENAI_ROUTE.to_owned(),
    });
    assert_eq!(error.class(), ProviderErrorClass::Authentication);
    assert!(error.to_string().contains(OPENAI_ROUTE));
    assert!(!error.to_string().contains(SECRET));
}

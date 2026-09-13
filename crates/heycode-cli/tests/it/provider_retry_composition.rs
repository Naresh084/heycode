//! P08 retry and durable-request proofs through the production plugin loader.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_cli::testing::RealCompositionHarness;
use heycode_http::{
    HttpErrorMetadata, HttpMethod, HttpRetryAfter, HttpSseRequest, HttpTransport, SseEvent,
    SseEventStream, TransportError,
};
use heycode_llm::{
    CapabilitySupport, CatalogSnapshot, DeepSeekProvider, ModelCapabilities, ModelDescriptor,
    ModelLifecycle, OpenRouterProvider, ProviderProtocol,
};
use heycode_provider_openrouter::{OpenRouterTransformPolicy, OpenRouterTransformRequestContext};
use heycode_session::{
    ProjectedInput, RequestAuthenticationSnapshot, RequestTargetSnapshot, Session,
    SessionEventKind, TurnEndReason, project_requests,
};
use tokio_util::sync::CancellationToken;

const CATALOG_REVISION: u64 = 7;
const CATALOG_FETCHED_AT_MS: u64 = 1_800_000_000_000;

type Attempt = Vec<Result<SseEvent, TransportError>>;

struct AttemptTransport {
    attempts: Mutex<VecDeque<Attempt>>,
    calls: AtomicUsize,
}

impl AttemptTransport {
    fn new(attempts: Vec<Attempt>) -> Arc<Self> {
        Arc::new(Self {
            attempts: Mutex::new(attempts.into()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl HttpTransport for AttemptTransport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.method(), HttpMethod::Post);
        assert_eq!(request.url(), "https://api.deepseek.com/chat/completions");
        assert!(
            request
                .headers()
                .iter()
                .any(|header| header.name() == "authorization")
        );
        let body: serde_json::Value =
            serde_json::from_slice(request.body().expect("DeepSeek request body")).unwrap();
        assert_eq!(body["model"], DeepSeekProvider::DEFAULT_MODEL);
        assert_eq!(body["stream"], true);

        if cancellation.is_cancelled() {
            return Box::pin(futures::stream::once(async {
                Err(TransportError::Cancelled)
            }));
        }
        let attempt = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                vec![Err(TransportError::Network {
                    message: "unexpected extra transport attempt".to_owned(),
                })]
            });
        Box::pin(futures::stream::iter(attempt))
    }
}

fn deepseek_v4_snapshot() -> CatalogSnapshot {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.reasoning = CapabilitySupport::Supported;
    capabilities.prompt_cache = CapabilitySupport::Supported;
    CatalogSnapshot {
        provider: DeepSeekProvider::setup_profile().descriptor,
        models: vec![ModelDescriptor {
            pricing: heycode_llm::ModelPricing::unknown(),
            performance: heycode_llm::ModelPerformance::unknown(),
            id: DeepSeekProvider::DEFAULT_MODEL.to_owned(),
            display_name: "DeepSeek V4 Flash".to_owned(),
            aliases: Vec::new(),
            created_at_ms: None,
            context_window: Some(1_048_576),
            max_output_tokens: Some(384 * 1024),
            lifecycle: ModelLifecycle::preview(),
            capabilities,
            reasoning: None,
        }],
        revision: CATALOG_REVISION,
        fetched_at_ms: CATALOG_FETCHED_AT_MS,
    }
}

fn event(value: serde_json::Value) -> Result<SseEvent, TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: value.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn done() -> Result<SseEvent, TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: "[DONE]".to_owned(),
        id: None,
        retry_ms: None,
    })
}

fn success(response_id: &str, text: &str) -> Attempt {
    vec![
        event(serde_json::json!({
            "id": response_id,
            "choices": [{
                "index": 0,
                "delta": {"content": text},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 11, "completion_tokens": 3}
        })),
        done(),
    ]
}

fn http_error(
    status: u16,
    retry_after: Option<HttpRetryAfter>,
) -> Result<SseEvent, TransportError> {
    Err(TransportError::http(
        status,
        r#"{"error":{"message":"provider-secret-canary"}}"#,
        HttpErrorMetadata::new(retry_after, None),
    ))
}

async fn wait_for_calls(transport: &AttemptTransport, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if transport.calls() == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

struct OpenRouterTransport {
    calls: AtomicUsize,
}

impl HttpTransport for OpenRouterTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(
            request.url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        let body: serde_json::Value =
            serde_json::from_slice(request.body().expect("OpenRouter request body")).unwrap();
        assert_eq!(body["model"], OpenRouterProvider::DEFAULT_MODEL);
        assert_eq!(body["reasoning"]["effort"], "max");
        assert_eq!(body["provider"]["allow_fallbacks"], true);
        assert_eq!(body["provider"]["require_parameters"], false);
        assert_eq!(body["provider"]["data_collection"], "allow");
        assert_eq!(
            body["plugins"],
            serde_json::json!([
                {"id":"context-compression","enabled":false},
                {"id":"file-parser","enabled":false},
                {"id":"response-healing","enabled":false}
            ])
        );
        let tools = body["tools"].as_array().unwrap();
        // Production search has no portable implementation. Prefer-local may
        // select the compatible native candidate, but cannot invent a fallback.
        assert_eq!(body["max_tool_calls"], 5);
        assert!(tools.iter().any(|tool| {
            tool["type"] == "openrouter:web_search"
                && tool["parameters"]["engine"] == "auto"
                && tool["parameters"]["max_results"] == 5
                && tool["parameters"]["max_characters"] == 4_000
        }));
        assert!(!tools.iter().any(|tool| {
            tool["type"] == "function" && tool["function"]["name"] == "web_search"
        }));
        let first = [
            event(serde_json::json!({
                "id":"openrouter-1",
                "choices":[{"index":0,"delta":{"reasoning":"plan"},"finish_reason":null}]
            })),
            event(serde_json::json!({
                "id":"openrouter-1",
                "choices":[{"index":0,"delta":{
                    "content":"ready",
                    "annotations":[{
                        "type":"url_citation",
                        "url_citation":{
                            "url":"https://example.test/rust",
                            "title":"Rust",
                            "content":"bounded source",
                            "start_index":0,
                            "end_index":5
                        }
                    }]
                },"finish_reason":"stop"}],
                "usage":{
                    "prompt_tokens":5,"completion_tokens":2,
                    "server_tool_use":{"web_search_requests":1}
                }
            })),
            done(),
        ];
        let local = [
            event(serde_json::json!({
                "id":"openrouter-2",
                "choices":[{"index":0,"delta":{"reasoning":"plan"},"finish_reason":null}]
            })),
            event(serde_json::json!({
                "id":"openrouter-2",
                "choices":[{"index":0,"delta":{"content":"local-ready"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":2}
            })),
            done(),
        ];
        Box::pin(futures::stream::iter(if call_number == 1 {
            first
        } else {
            local
        }))
    }
}

fn openrouter_snapshot() -> CatalogSnapshot {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.reasoning = CapabilitySupport::Supported;
    capabilities.image_input = CapabilitySupport::Supported;
    capabilities.structured_output = CapabilitySupport::Supported;
    capabilities.prompt_cache = CapabilitySupport::Supported;
    capabilities.native_web = CapabilitySupport::Supported;
    CatalogSnapshot {
        provider: OpenRouterProvider::setup_profile().descriptor,
        models: vec![ModelDescriptor {
            pricing: heycode_llm::ModelPricing::unknown(),
            performance: heycode_llm::ModelPerformance::unknown(),
            id: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
            display_name: "Z.ai: GLM 5.3 Flash".to_owned(),
            aliases: Vec::new(),
            created_at_ms: None,
            context_window: Some(1_048_576),
            max_output_tokens: Some(131_072),
            lifecycle: ModelLifecycle::stable(),
            capabilities,
            reasoning: None,
        }],
        revision: CATALOG_REVISION,
        fetched_at_ms: CATALOG_FETCHED_AT_MS,
    }
}

fn compose_deepseek(transport: Arc<AttemptTransport>) -> heycode_cli::testing::ComposedTestWorld {
    let provider = Arc::new(
        DeepSeekProvider::from_key_with_transport(
            "test-key",
            None,
            heycode_http::HttpService::new(transport),
        )
        .unwrap(),
    );
    let harness = RealCompositionHarness::new().unwrap();
    harness
        .seed_catalog_snapshot(deepseek_v4_snapshot())
        .unwrap();
    harness.with_provider(provider).compose().unwrap()
}

#[tokio::test]
async fn production_loader_dispatches_verified_openrouter_policy_and_default_reasoning() {
    let transport = Arc::new(OpenRouterTransport {
        calls: AtomicUsize::new(0),
    });
    let provider = Arc::new(
        OpenRouterProvider::from_key_with_transport(
            "test-key",
            Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
            heycode_http::HttpService::new(transport.clone()),
            vec![
                OpenRouterTransformPolicy::all_disabled()
                    .provider_option(OpenRouterTransformRequestContext::new(true, false))
                    .unwrap(),
            ],
        )
        .unwrap(),
    );
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.model = OpenRouterProvider::DEFAULT_MODEL.to_owned();
    harness
        .seed_catalog_snapshot(openrouter_snapshot())
        .unwrap();
    let world = harness.with_provider(provider).compose().unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    let report = agent.send("hello").await.unwrap();
    assert_eq!(report.text, "ready");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

    let session = world
        .context()
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let requests = {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        project_requests(session.events()).unwrap()
    };
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.header.provider, OpenRouterProvider::NAME);
    assert_eq!(request.header.model, OpenRouterProvider::DEFAULT_MODEL);
    assert_eq!(
        request.header.options.reasoning_effort.as_deref(),
        Some("max")
    );
    assert!(request.header.options.defaulted_reasoning_effort);
    assert_eq!(request.header.options.provider_options.len(), 3);
    assert_eq!(request.header.options.provider_options[1].kind(), "caching");
    assert_eq!(
        request.header.options.provider_options[1].data(),
        &serde_json::json!({"type":"ephemeral"})
    );
    assert_eq!(
        request.header.options.provider_options[0].data()["data_collection"],
        "allow"
    );
    assert_eq!(
        request.header.options.provider_options[2].data()["plugins"],
        serde_json::json!([
            {"id":"context-compression","enabled":false},
            {"id":"file-parser","enabled":false},
            {"id":"response-healing","enabled":false}
        ])
    );
    assert_eq!(
        request
            .header
            .options
            .native_tool_routes
            .iter()
            .map(|route| (route.logical(), route.implementation()))
            .collect::<Vec<_>>(),
        [
            ("web_fetch", "client:web_fetch"),
            ("web_search", "openrouter:web_search"),
        ]
    );
    assert_eq!(request.header.options.native_features, ["web"]);
    assert!(
        !request
            .header
            .tools
            .iter()
            .any(|tool| tool.name == "web_search")
    );
    assert!(matches!(
        request.server_tool_events.as_slice(),
        [
            heycode_session::ProjectedServerToolEvent::Citation { citation, .. },
            heycode_session::ProjectedServerToolEvent::Usage { usage, .. }
        ]
            if citation.url() == "https://example.test/rust"
                && citation.cited_text() == Some("bounded source")
                && usage.logical() == "web_search"
                && usage.requests() == 1
    ));

    let settings = world
        .context()
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let namespace = heycode_native_tools::native_tool_policy_namespace().unwrap();
    let initial = settings.get(&namespace).unwrap().unwrap();
    settings
        .replace_user(
            &namespace,
            serde_json::json!({"default":"prefer-local","overrides":{}}),
            Some(initial.revision()),
        )
        .unwrap();
    let local = agent
        .send("search after changing preference")
        .await
        .unwrap();
    assert_eq!(local.text, "local-ready");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    let after_local = {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        project_requests(session.events()).unwrap()
    };
    let local_request = &after_local[1];
    assert_eq!(local_request.header.options.native_features, ["web"]);
    assert!(
        !local_request
            .header
            .tools
            .iter()
            .any(|tool| tool.name == "web_search")
    );
    assert_eq!(
        local_request.header.options.native_tool_routes[1].implementation(),
        "openrouter:web_search"
    );

    let current = settings.get(&namespace).unwrap().unwrap();
    settings
        .replace_user(
            &namespace,
            serde_json::json!({
                "default":"prefer-native",
                "overrides":{"web_fetch":"native-only"}
            }),
            Some(current.revision()),
        )
        .unwrap();
    let error = agent
        .send("refuse unsupported native fetch")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("web_fetch"));
    assert!(error.to_string().contains("native-only"));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    world.shutdown();
}

#[tokio::test]
async fn production_loader_retries_503_and_replays_durable_deepseek_state() {
    let transport = AttemptTransport::new(vec![
        vec![http_error(503, None)],
        success("response-1", "recovered"),
        success("response-2", "continued"),
    ]);
    let world = compose_deepseek(transport.clone());
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    let first = tokio::time::timeout(Duration::from_secs(3), agent.send("first"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.text, "recovered");
    assert_eq!(transport.calls(), 2);

    let second = agent.send("second").await.unwrap();
    assert_eq!(second.text, "continued");
    assert_eq!(transport.calls(), 3);

    let session = world
        .context()
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let (session_dir, live_events, live_requests) = {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        let session_dir = session.path().parent().unwrap().to_path_buf();
        let events = session.events().to_vec();
        let requests = project_requests(session.events()).unwrap();
        (session_dir, events, requests)
    };
    assert_eq!(live_requests.len(), 2);

    let first = &live_requests[0];
    assert_eq!(first.header.provider, DeepSeekProvider::NAME);
    assert_eq!(first.header.model, DeepSeekProvider::DEFAULT_MODEL);
    assert_eq!(
        first.header.protocol,
        ProviderProtocol::OpenAiChatCompletions
    );
    assert_eq!(
        first.header.target,
        RequestTargetSnapshot::Http {
            base_url: DeepSeekProvider::BASE_URL.to_owned()
        }
    );
    assert_eq!(
        first.header.authentication,
        RequestAuthenticationSnapshot::AdapterOwned
    );
    assert_eq!(first.context.catalog_revision, Some(CATALOG_REVISION));
    assert_eq!(
        first.context.catalog_fetched_at_ms,
        Some(CATALOG_FETCHED_AT_MS)
    );
    assert_eq!(first.context.context_window, Some(1_048_576));
    assert_eq!(first.context.max_output_tokens, Some(384 * 1024));

    let provider_items = live_events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AssistantProviderItem {
                request_id,
                output_index,
                item,
                ..
            } => Some((request_id, output_index, item)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(provider_items.len(), 2);
    assert_eq!(provider_items[0].0, &first.request_id);
    assert_eq!(*provider_items[0].1, 0);
    assert_eq!(
        provider_items[0].2.data().get("content"),
        Some(&serde_json::json!("recovered"))
    );

    assert!(live_requests[1].inputs.iter().any(|input| {
        matches!(input, ProjectedInput::ProviderState(item)
            if item.provider() == DeepSeekProvider::NAME
                && item.model() == DeepSeekProvider::DEFAULT_MODEL
                && item.data().get("content") == Some(&serde_json::json!("recovered")))
    }));

    let replayed = Session::open(session_dir).unwrap();
    assert_eq!(project_requests(replayed.events()).unwrap(), live_requests);
}

#[tokio::test]
async fn production_loader_cancels_retry_after_without_a_second_attempt() {
    let transport = AttemptTransport::new(vec![
        vec![http_error(
            429,
            Some(HttpRetryAfter::Delay(Duration::from_secs(60))),
        )],
        success("must-not-run", "duplicate"),
    ]);
    let world = compose_deepseek(transport.clone());
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let cancellation = CancellationToken::new();
    let turn = {
        let agent = agent.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { agent.send_cancellable("cancel retry", cancellation).await })
    };

    wait_for_calls(&transport, 1).await;
    assert_eq!(transport.calls(), 1);
    cancellation.cancel();
    let report = tokio::time::timeout(Duration::from_secs(1), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.reason, "aborted");
    assert_eq!(transport.calls(), 1);

    let session = world
        .context()
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let session = session.lock().unwrap_or_else(|error| error.into_inner());
    assert!(session.events().iter().any(|event| matches!(
        event.kind,
        SessionEventKind::TurnEnd {
            reason: TurnEndReason::Aborted,
            ..
        }
    )));
}

#[tokio::test]
async fn production_loader_never_retries_after_stream_output() {
    let transport = AttemptTransport::new(vec![
        vec![
            event(serde_json::json!({
                "id": "response-partial",
                "choices": [{
                    "index": 0,
                    "delta": {"content": "partial"},
                    "finish_reason": null
                }]
            })),
            Err(TransportError::Network {
                message: "network-secret-canary".to_owned(),
            }),
        ],
        success("must-not-run", "duplicate"),
    ]);
    let world = compose_deepseek(transport.clone());
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    let error = agent.send("partial failure").await.unwrap_err().to_string();
    assert_eq!(transport.calls(), 1);
    assert!(!error.contains("network-secret-canary"), "{error}");
    assert!(!error.contains("provider-secret-canary"), "{error}");

    let session = world
        .context()
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let session = session.lock().unwrap_or_else(|error| error.into_inner());
    assert!(session.events().iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::AssistantChunk {
            text: Some(text),
            ..
        } if text == "partial"
    )));
    assert!(session.events().iter().any(|event| matches!(
        event.kind,
        SessionEventKind::TurnEnd {
            reason: TurnEndReason::Error,
            ..
        }
    )));
}

//! POR05 OpenRouter server-web request and citation replay boundary.
//!
//! The deterministic Chat annotation frames below are Synthetic. OpenRouter's
//! current guide documents final `message.annotations`, while its generated
//! `ChatStreamDelta` schema does not document a streamed annotation location.
//! Only an authenticated raw capture can promote that wire-placement claim.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::{
    NativeToolImplementationKind, NativeToolRoute, ProviderProtocol, ProviderStateKind,
};
use heycode_credentials::CredentialSecret;
use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, ReqwestHttpTransport, SseEvent, SseEventStream,
};
use heycode_llm::testing::{ConformanceFixtureMetadata, ConformanceSourceKind};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, NativeFeature, OpenRouterProvider, Provider, RequestDraft, RequestedCapability,
    ResolveError, RetrySafety,
};
use heycode_provider_openrouter::{
    OPENROUTER_WEB_SEARCH_IMPLEMENTATION, OpenRouterTransformPolicy,
    OpenRouterTransformRequestContext,
};
use tokio_util::sync::CancellationToken;

const WEB_SEARCH_GUIDE: &str = "https://openrouter.ai/docs/guides/features/server-tools/web-search";
const STREAMING_GUIDE: &str = "https://openrouter.ai/docs/api/reference/streaming";
const SYNTHETIC_FIXTURE_RECONCILED_AT_MS: u64 = 1_788_123_354_000;

fn synthetic_chat_metadata() -> ConformanceFixtureMetadata {
    ConformanceFixtureMetadata::new(
        OpenRouterProvider::NAME,
        ConformanceSourceKind::Synthetic,
        WEB_SEARCH_GUIDE,
        "web-search-guide-2026-08-31",
        SYNTHETIC_FIXTURE_RECONCILED_AT_MS,
    )
    .unwrap()
}

struct ScriptTransport {
    scripts: Mutex<Vec<Vec<SseEvent>>>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl HttpTransport for ScriptTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap()).unwrap());
        let script = self.scripts.lock().unwrap().remove(0);
        Box::pin(futures::stream::iter(script.into_iter().map(Ok)))
    }
}

fn sse(data: serde_json::Value) -> SseEvent {
    SseEvent {
        event: "message".to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    }
}

fn done() -> SseEvent {
    SseEvent {
        event: "message".to_owned(),
        data: "[DONE]".to_owned(),
        id: None,
        retry_ms: None,
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
        display_name: "GLM 5.3 Flash".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(131_072),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            reasoning: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn provider_with_http(http: HttpService) -> OpenRouterProvider {
    OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        http,
        vec![
            OpenRouterTransformPolicy::all_disabled()
                .provider_option(OpenRouterTransformRequestContext::new(true, false))
                .unwrap(),
        ],
    )
    .unwrap()
}

fn scripted_provider(
    scripts: Vec<Vec<SseEvent>>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
) -> OpenRouterProvider {
    provider_with_http(HttpService::new(Arc::new(ScriptTransport {
        scripts: Mutex::new(scripts),
        bodies,
    })))
}

fn draft(provider: &OpenRouterProvider, inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: OpenRouterProvider::NAME.to_owned(),
        model: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs,
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: vec![NativeFeature::Web],
        native_tool_routes: vec![
            NativeToolRoute::new(
                "web_search",
                OPENROUTER_WEB_SEARCH_IMPLEMENTATION,
                NativeToolImplementationKind::Provider,
                Some(OpenRouterProvider::NAME.to_owned()),
            )
            .unwrap(),
        ],
        provider_options: Provider::request_options(provider),
        temperature: None,
        max_output_tokens: Some(1024),
        purpose: CallPurpose::Conversation,
    }
}

#[tokio::test]
async fn synthetic_chat_citation_normalizes_and_replays_without_inventing_a_search_call() {
    let metadata = synthetic_chat_metadata();
    assert_eq!(metadata.source_kind(), ConformanceSourceKind::Synthetic);
    assert_eq!(metadata.source(), WEB_SEARCH_GUIDE);
    let streaming = ConformanceFixtureMetadata::new(
        OpenRouterProvider::NAME,
        ConformanceSourceKind::OfficialExample,
        STREAMING_GUIDE,
        "chat-usage-repeat-2026-08-31",
        SYNTHETIC_FIXTURE_RECONCILED_AT_MS,
    )
    .unwrap();
    assert_eq!(
        streaming.source_kind(),
        ConformanceSourceKind::OfficialExample
    );
    assert_eq!(streaming.source(), STREAMING_GUIDE);
    let annotation = serde_json::json!({
        "type":"url_citation",
        "url_citation":{
            "url":"https://example.test/source",
            "title":"Primary source",
            "content":"bounded excerpt",
            "start_index":0,
            "end_index":15
        }
    });
    let scripts = vec![
        vec![
            sse(serde_json::json!({
                "id":"web_1",
                "choices":[{"index":0,"delta":{
                    "content":"Grounded answer",
                    "annotations":[annotation.clone()]
                },"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"web_1",
                "choices":[{"index":0,"delta":{"content":"","role":"assistant"},
                    "finish_reason":"stop","native_finish_reason":"stop"}]
            })),
            sse(serde_json::json!({
                "id":"web_1",
                "choices":[{"index":0,"delta":{"content":"","role":"assistant"},
                    "finish_reason":"stop","native_finish_reason":"stop"}],
                "usage":{
                    "prompt_tokens":10,
                    "completion_tokens":4,
                    "server_tool_use_details":{"web_search_requests":1}
                }
            })),
            done(),
        ],
        vec![
            sse(serde_json::json!({
                "id":"web_2",
                "choices":[{"index":0,"delta":{"content":"Replayed"},"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"web_2",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
            done(),
        ],
    ];
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let provider = scripted_provider(scripts, bodies.clone());

    let first = InferenceAdapter::resolve(
        &provider,
        draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("search"))],
        ),
        &model(),
    )
    .unwrap();
    assert_eq!(first.retry_spec().safety(), RetrySafety::Never);
    let first_events = InferenceAdapter::stream(&provider, first)
        .collect::<Vec<_>>()
        .await;
    assert!(first_events.iter().all(Result::is_ok), "{first_events:#?}");
    let first_events = first_events
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(first_events.iter().any(|event| matches!(
        event,
        InferenceEvent::Citation { output_index: 0, citation }
            if citation.url() == "https://example.test/source"
                && citation.title() == Some("Primary source")
                && citation.cited_text() == Some("bounded excerpt")
                && citation.start_index() == Some(0)
                && citation.end_index() == Some(15)
    )));
    assert!(first_events.iter().any(|event| matches!(
        event,
        InferenceEvent::ServerToolUsage(usage)
            if usage.logical() == "web_search"
                && usage.requests() == 1
                && usage.evidence() == heycode_core::ServerToolUsageEvidence::ProviderAggregate
                && usage.cost() == &heycode_core::ServerToolUsageCost::Unknown
    )));
    assert!(
        !first_events.iter().any(|event| matches!(
            event,
            InferenceEvent::ServerToolCall { .. } | InferenceEvent::ServerToolResult { .. }
        )),
        "aggregate usage is not exact per-call identity"
    );
    let state = first_events
        .iter()
        .find_map(|event| match event {
            InferenceEvent::ProviderState(state) => Some(state.clone()),
            _ => None,
        })
        .expect("the complete assistant state is retained for replay");
    assert_eq!(state.provider(), OpenRouterProvider::NAME);
    assert_eq!(state.model(), OpenRouterProvider::DEFAULT_MODEL);
    assert_eq!(state.protocol(), ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(state.kind(), ProviderStateKind::ChatAssistantMessage);
    assert_eq!(state.schema_version(), 1);
    assert_eq!(state.data()["annotations"], serde_json::json!([annotation]));
    assert!(state.data().get("tool_calls").is_none());

    let server_usage_index = first_events
        .iter()
        .position(|event| matches!(event, InferenceEvent::ServerToolUsage(_)))
        .unwrap();
    let token_usage_index = first_events
        .iter()
        .position(|event| matches!(event, InferenceEvent::Usage(_)))
        .unwrap();
    let finish_index = first_events
        .iter()
        .position(|event| matches!(event, InferenceEvent::Finish(_)))
        .unwrap();
    assert!(server_usage_index < token_usage_index);
    assert!(token_usage_index < finish_index);

    let second = InferenceAdapter::resolve(
        &provider,
        draft(
            &provider,
            vec![
                InferenceInput::ProviderState(state.clone()),
                InferenceInput::Message(ChatMessage::user("use that source")),
            ],
        ),
        &model(),
    )
    .unwrap();
    let second_events = InferenceAdapter::stream(&provider, second)
        .collect::<Vec<_>>()
        .await;
    assert!(
        second_events.iter().all(Result::is_ok),
        "{second_events:#?}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies[0]["tools"],
        serde_json::json!([{
            "type":"openrouter:web_search",
            "parameters":{
                "engine":"auto",
                "max_results":5,
                "max_uses":3,
                "max_total_results":15,
                "max_characters":4000
            }
        }])
    );
    assert_eq!(bodies[0]["max_tool_calls"], 5);
    assert!(
        bodies[0]["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .all(|plugin| plugin["id"] != "web"),
        "the deprecated web plugin must not accompany the server tool"
    );
    assert_eq!(
        bodies[1]["messages"][0]["annotations"],
        state.data()["annotations"],
        "citation annotations must replay byte-semantically"
    );
    assert!(bodies[1]["messages"][0].get("tool_calls").is_none());
}

#[test]
fn native_web_requires_affirmative_catalog_evidence() {
    let provider = scripted_provider(Vec::new(), Arc::new(Mutex::new(Vec::new())));
    for (support, expected_unproven) in [
        (CapabilitySupport::Unknown, true),
        (CapabilitySupport::Unsupported, false),
    ] {
        let mut descriptor = model();
        descriptor.capabilities.native_web = support;
        let error = InferenceAdapter::resolve(
            &provider,
            draft(
                &provider,
                vec![InferenceInput::Message(ChatMessage::user("search"))],
            ),
            &descriptor,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                ResolveError::Unproven {
                    capability: RequestedCapability::NativeWeb,
                    ..
                }
            ) == expected_unproven,
            "Unknown and Unsupported must remain distinct: {error}"
        );
        assert!(
            matches!(
                error,
                ResolveError::Unsupported {
                    capability: RequestedCapability::NativeWeb,
                    ..
                }
            ) != expected_unproven,
            "Unknown and Unsupported must remain distinct: {error}"
        );
    }
}

#[tokio::test]
async fn provider_aggregate_above_the_shared_server_tool_budget_never_settles() {
    let scripts = vec![vec![
        sse(serde_json::json!({
            "id":"web_over_budget",
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":10,
                "completion_tokens":4,
                "server_tool_use":{"web_search_requests":6}
            }
        })),
        done(),
    ]];
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let provider = scripted_provider(scripts, bodies.clone());
    let call = InferenceAdapter::resolve(
        &provider,
        draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("search"))],
        ),
        &model(),
    )
    .unwrap();
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    let errors = events
        .iter()
        .filter_map(|event| event.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("budget"));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_)
            | InferenceEvent::ServerToolUsage(_)
            | InferenceEvent::Usage(_)
            | InferenceEvent::Finish(_))
    )));
    assert_eq!(bodies.lock().unwrap()[0]["max_tool_calls"], 5);
}

fn aggregate_script(guide_requests: Option<u64>, schema_requests: Option<u64>) -> Vec<SseEvent> {
    let mut usage = serde_json::Map::new();
    usage.insert("prompt_tokens".to_owned(), serde_json::json!(10));
    usage.insert("completion_tokens".to_owned(), serde_json::json!(4));
    if let Some(requests) = guide_requests {
        usage.insert(
            "server_tool_use".to_owned(),
            serde_json::json!({"web_search_requests":requests}),
        );
    }
    if let Some(requests) = schema_requests {
        usage.insert(
            "server_tool_use_details".to_owned(),
            serde_json::json!({"web_search_requests":requests}),
        );
    }
    vec![
        sse(serde_json::json!({
            "id":"web_usage_alias",
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":serde_json::Value::Object(usage)
        })),
        done(),
    ]
}

#[tokio::test]
async fn documented_aggregate_aliases_normalize_only_when_equal() {
    for (guide, schema, expected) in [
        (Some(1), None, 1),
        (None, Some(2), 2),
        (Some(3), Some(3), 3),
    ] {
        let provider = scripted_provider(
            vec![aggregate_script(guide, schema)],
            Arc::new(Mutex::new(Vec::new())),
        );
        let call = InferenceAdapter::resolve(
            &provider,
            draft(
                &provider,
                vec![InferenceInput::Message(ChatMessage::user("search"))],
            ),
            &model(),
        )
        .unwrap();
        let events = InferenceAdapter::stream(&provider, call)
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:#?}");
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    Ok(InferenceEvent::ServerToolUsage(usage)) => Some(usage.requests()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![expected]
        );
    }

    let provider = scripted_provider(
        vec![aggregate_script(Some(1), Some(2))],
        Arc::new(Mutex::new(Vec::new())),
    );
    let call = InferenceAdapter::resolve(
        &provider,
        draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("search"))],
        ),
        &model(),
    )
    .unwrap();
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    let errors = events
        .iter()
        .filter_map(|event| event.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("aliases"));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_)
            | InferenceEvent::ServerToolUsage(_)
            | InferenceEvent::Usage(_)
            | InferenceEvent::Finish(_))
    )));
}

#[tokio::test]
async fn generic_server_tool_details_do_not_invent_web_usage() {
    let scripts = vec![vec![
        sse(serde_json::json!({
            "id":"generic_server_usage",
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":10,
                "completion_tokens":4,
                "server_tool_use_details":{
                    "tool_calls_requested":2,
                    "tool_calls_executed":2
                }
            }
        })),
        done(),
    ]];
    let provider = scripted_provider(scripts, Arc::new(Mutex::new(Vec::new())));
    let call = InferenceAdapter::resolve(
        &provider,
        draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("search"))],
        ),
        &model(),
    )
    .unwrap();
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:#?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::ServerToolUsage(_))))
    );
}

#[derive(Debug, Default)]
struct RawLiveWebEvidence {
    citation_annotations: usize,
    aggregate_requests: Option<u64>,
    duplicate_aggregate: bool,
    conflicting_aggregate_aliases: bool,
}

struct EvidenceTransport {
    inner: ReqwestHttpTransport,
    evidence: Arc<Mutex<RawLiveWebEvidence>>,
}

impl HttpTransport for EvidenceTransport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        let evidence = self.evidence.clone();
        let stream = self.inner.sse(request, cancellation);
        Box::pin(stream.map(move |event| {
            if let Ok(frame) = &event {
                observe_live_frame(&evidence, frame);
            }
            event
        }))
    }
}

fn observe_live_frame(evidence: &Arc<Mutex<RawLiveWebEvidence>>, frame: &SseEvent) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&frame.data) else {
        return;
    };
    let citation_annotations = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|choice| {
            choice
                .get("delta")
                .and_then(|delta| delta.get("annotations"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|annotation| {
            annotation.get("type").and_then(serde_json::Value::as_str) == Some("url_citation")
        })
        .count();
    let aggregate_aliases = value.get("usage").map(|usage| {
        let guide = usage
            .get("server_tool_use")
            .and_then(|usage| usage.get("web_search_requests"))
            .and_then(serde_json::Value::as_u64);
        let schema = usage
            .get("server_tool_use_details")
            .and_then(|usage| usage.get("web_search_requests"))
            .and_then(serde_json::Value::as_u64);
        (guide, schema)
    });
    let mut evidence = evidence.lock().unwrap();
    evidence.citation_annotations = evidence
        .citation_annotations
        .saturating_add(citation_annotations);
    let aggregate = match aggregate_aliases {
        Some((Some(guide), Some(schema))) if guide != schema => {
            evidence.conflicting_aggregate_aliases = true;
            None
        }
        Some((Some(guide), _)) => Some(guide),
        Some((_, Some(schema))) => Some(schema),
        _ => None,
    };
    if let Some(aggregate) = aggregate
        && evidence.aggregate_requests.replace(aggregate).is_some()
    {
        evidence.duplicate_aggregate = true;
    }
}

#[test]
fn live_observer_retains_only_closed_counts_and_flags() {
    let evidence = Arc::new(Mutex::new(RawLiveWebEvidence::default()));
    observe_live_frame(
        &evidence,
        &sse(serde_json::json!({
            "choices":[{"delta":{"annotations":[{
                "type":"url_citation",
                "url_citation":{
                    "url":"https://secret.example.test/path",
                    "title":"private title",
                    "content":"private excerpt"
                }
            }]}}],
            "usage":{"server_tool_use":{"web_search_requests":2}}
        })),
    );
    let evidence = evidence.lock().unwrap();
    assert_eq!(evidence.citation_annotations, 1);
    assert_eq!(evidence.aggregate_requests, Some(2));
    assert!(!evidence.duplicate_aggregate);
    assert!(!evidence.conflicting_aggregate_aliases);
    let debug = format!("{evidence:?}");
    for private in ["secret.example", "private title", "private excerpt"] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn live_observer_accepts_equal_documented_usage_aliases_and_flags_conflicts() {
    for (guide, schema, expected, conflict) in [
        (Some(2), None, Some(2), false),
        (None, Some(3), Some(3), false),
        (Some(4), Some(4), Some(4), false),
        (Some(5), Some(6), None, true),
    ] {
        let evidence = Arc::new(Mutex::new(RawLiveWebEvidence::default()));
        let mut usage = serde_json::Map::new();
        if let Some(guide) = guide {
            usage.insert(
                "server_tool_use".to_owned(),
                serde_json::json!({"web_search_requests":guide}),
            );
        }
        if let Some(schema) = schema {
            usage.insert(
                "server_tool_use_details".to_owned(),
                serde_json::json!({"web_search_requests":schema}),
            );
        }
        observe_live_frame(
            &evidence,
            &sse(serde_json::json!({"usage":serde_json::Value::Object(usage)})),
        );
        let evidence = evidence.lock().unwrap();
        assert_eq!(evidence.aggregate_requests, expected);
        assert_eq!(evidence.conflicting_aggregate_aliases, conflict);
    }
}

enum LiveWebGate {
    Skipped(&'static str),
    Ready(CredentialSecret),
}

fn live_web_gate(opt_in: Option<String>, key: Option<String>) -> LiveWebGate {
    if opt_in.as_deref() != Some("1") {
        return LiveWebGate::Skipped("HEYCODE_E2E is not set to 1");
    }
    match key {
        Some(key) if !key.trim().is_empty() => LiveWebGate::Ready(CredentialSecret::new(key)),
        _ => LiveWebGate::Skipped("OPENROUTER_API_KEY is unset or blank"),
    }
}

#[test]
fn live_web_gate_is_explicit_and_credential_blind() {
    assert!(matches!(
        live_web_gate(None, Some("test-key".to_owned())),
        LiveWebGate::Skipped("HEYCODE_E2E is not set to 1")
    ));
    assert!(matches!(
        live_web_gate(Some("1".to_owned()), None),
        LiveWebGate::Skipped("OPENROUTER_API_KEY is unset or blank")
    ));
    assert!(matches!(
        live_web_gate(Some("1".to_owned()), Some("   ".to_owned())),
        LiveWebGate::Skipped("OPENROUTER_API_KEY is unset or blank")
    ));
    let LiveWebGate::Ready(secret) =
        live_web_gate(Some("1".to_owned()), Some("test-key".to_owned()))
    else {
        panic!("explicit opt-in plus a nonblank key must open the live gate");
    };
    assert_eq!(format!("{secret:?}"), "CredentialSecret([REDACTED])");
}

#[tokio::test]
async fn live_chat_web_search_requires_documented_citation_and_aggregate_when_enabled() {
    let LiveWebGate::Ready(secret) = live_web_gate(
        std::env::var("HEYCODE_E2E").ok(),
        std::env::var(OpenRouterProvider::API_KEY_ENV).ok(),
    ) else {
        return;
    };
    let evidence = Arc::new(Mutex::new(RawLiveWebEvidence::default()));
    let transport = EvidenceTransport {
        inner: ReqwestHttpTransport::new().unwrap(),
        evidence: evidence.clone(),
    };
    let provider = OpenRouterProvider::from_key_with_transport(
        secret.expose().to_owned(),
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        HttpService::new(Arc::new(transport)),
        vec![
            OpenRouterTransformPolicy::all_disabled()
                .provider_option(OpenRouterTransformRequestContext::new(true, false))
                .unwrap(),
        ],
    )
    .unwrap();
    let call = InferenceAdapter::resolve(
        &provider,
        draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user(
                "Use web search before answering. Find OpenRouter's current web-search server-tool documentation title and cite that source.",
            ))],
        ),
        &model(),
    )
    .unwrap();
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    let failures = events
        .iter()
        .filter_map(|event| event.as_ref().err().map(ToString::to_string))
        .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "live OpenRouter web search returned body-free failures: {failures:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Citation { .. })))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ServerToolUsage(usage)) if usage.requests() > 0
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ServerToolCall { .. } | InferenceEvent::ServerToolResult { .. })
    )));

    let evidence = evidence.lock().unwrap();
    assert!(evidence.citation_annotations > 0);
    assert!(evidence.aggregate_requests.is_some_and(|count| count > 0));
    assert!(!evidence.duplicate_aggregate);
    assert!(!evidence.conflicting_aggregate_aliases);
}

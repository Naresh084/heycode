//! POR07 credential-safe OpenRouter text/tool/search/routing evidence lane.
//!
//! The production-composition QLIVE01 test owns catalog, reasoning, text and
//! exactly-once client-tool evidence. This provider-local follow-on accepts
//! only that fresh content-withheld artifact, then exercises the current
//! `openrouter:web_search` Chat route with an explicit routing policy. Its
//! combined artifact contains closed outcome metadata only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;
use heycode_core::{
    NativeToolImplementationKind, NativeToolRoute, ProviderStateKind, ServerToolUsageEvidence,
};
use heycode_credentials::CredentialSecret;
use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, ReqwestHttpTransport, SseEvent, SseEventStream,
};
use heycode_live_artifact::{
    ArtifactRecorder, FailureClass, LiveArtifact, LiveOutcome, RouteId, SkipReason,
};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, NativeFeature, OpenRouterDataCollection, OpenRouterProvider,
    OpenRouterRoutingPolicy, Provider, ProviderErrorClass, RequestDraft,
};
use heycode_provider_openrouter::{
    OPENROUTER_WEB_SEARCH_IMPLEMENTATION, OpenRouterTransformPolicy,
    OpenRouterTransformRequestContext,
};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "z-ai/glm-5.3-flash";
const PRODUCTION_ARTIFACT: &str = "openrouter-glm-5.3-flash.json";
const CONFORMANCE_ARTIFACT: &str = "openrouter-glm-5.3-flash-conformance.json";
const PRODUCTION_NOTE: &str = "live catalog, reasoning request, text turn and tool loop passed";
const CONFORMANCE_NOTE: &str = "catalog, text, tool, search and routing canaries passed";
const FRESHNESS: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ClosedRequestEvidence {
    exact_model: bool,
    explicit_routing: bool,
    exact_web_policy: bool,
    explicit_transform_policy: bool,
}

#[derive(Debug, Default)]
struct ClosedLaneEvidence {
    request_count: usize,
    request: ClosedRequestEvidence,
    citation_annotations: usize,
    aggregate_requests: Option<u64>,
    duplicate_aggregate: bool,
    conflicting_aggregate_aliases: bool,
}

fn inspect_request(body: &[u8]) -> ClosedRequestEvidence {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return ClosedRequestEvidence::default();
    };
    let routing = serde_json::json!({
        "allow_fallbacks":true,
        "require_parameters":true,
        "data_collection":"allow"
    });
    let web_tool = serde_json::json!([{
        "type":"openrouter:web_search",
        "parameters":{
            "engine":"auto",
            "max_results":5,
            "max_uses":3,
            "max_total_results":15,
            "max_characters":4000
        }
    }]);
    let transforms = serde_json::json!([
        {"id":"context-compression","enabled":false},
        {"id":"file-parser","enabled":false},
        {"id":"response-healing","enabled":false}
    ]);
    ClosedRequestEvidence {
        exact_model: value.get("model").and_then(serde_json::Value::as_str) == Some(MODEL)
            && value.get("stream").and_then(serde_json::Value::as_bool) == Some(true),
        explicit_routing: value.get("provider") == Some(&routing),
        exact_web_policy: value.get("tools") == Some(&web_tool)
            && value
                .get("max_tool_calls")
                .and_then(serde_json::Value::as_u64)
                == Some(5),
        explicit_transform_policy: value.get("plugins") == Some(&transforms),
    }
}

fn observe_request(evidence: &Arc<Mutex<ClosedLaneEvidence>>, request: &HttpSseRequest) {
    let request_evidence = request.body().map(inspect_request).unwrap_or_default();
    let mut evidence = evidence.lock().unwrap();
    evidence.request_count = evidence.request_count.saturating_add(1);
    evidence.request = request_evidence;
}

fn citation_annotations(value: &serde_json::Value) -> usize {
    value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|choice| {
            ["delta", "message"].into_iter().flat_map(move |container| {
                choice
                    .get(container)
                    .and_then(|container| container.get("annotations"))
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
        })
        .filter(|annotation| {
            annotation.get("type").and_then(serde_json::Value::as_str) == Some("url_citation")
        })
        .count()
}

fn aggregate_aliases(value: &serde_json::Value) -> (Option<u64>, Option<u64>) {
    let Some(usage) = value.get("usage") else {
        return (None, None);
    };
    let guide = usage
        .get("server_tool_use")
        .and_then(|usage| usage.get("web_search_requests"))
        .and_then(serde_json::Value::as_u64);
    let schema = usage
        .get("server_tool_use_details")
        .and_then(|usage| usage.get("web_search_requests"))
        .and_then(serde_json::Value::as_u64);
    (guide, schema)
}

fn observe_frame(evidence: &Arc<Mutex<ClosedLaneEvidence>>, frame: &SseEvent) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&frame.data) else {
        return;
    };
    let citations = citation_annotations(&value);
    let (guide, schema) = aggregate_aliases(&value);
    let mut evidence = evidence.lock().unwrap();
    evidence.citation_annotations = evidence.citation_annotations.saturating_add(citations);
    let aggregate = match (guide, schema) {
        (Some(guide), Some(schema)) if guide != schema => {
            evidence.conflicting_aggregate_aliases = true;
            None
        }
        (Some(guide), _) => Some(guide),
        (_, Some(schema)) => Some(schema),
        (None, None) => None,
    };
    if let Some(aggregate) = aggregate
        && evidence.aggregate_requests.replace(aggregate).is_some()
    {
        evidence.duplicate_aggregate = true;
    }
}

struct EvidenceTransport {
    inner: ReqwestHttpTransport,
    evidence: Arc<Mutex<ClosedLaneEvidence>>,
}

impl HttpTransport for EvidenceTransport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        observe_request(&self.evidence, &request);
        let evidence = self.evidence.clone();
        let stream = self.inner.sse(request, cancellation);
        Box::pin(stream.map(move |event| {
            if let Ok(frame) = &event {
                observe_frame(&evidence, frame);
            }
            event
        }))
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: MODEL.to_owned(),
        display_name: "GLM 5.3 Flash".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(131_072),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft(provider: &OpenRouterProvider) -> RequestDraft {
    RequestDraft {
        provider: OpenRouterProvider::NAME.to_owned(),
        model: MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user(
            "Use web search before answering. Identify the current title of OpenRouter's web-search server-tool guide and cite that source.",
        ))],
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
        max_output_tokens: Some(1_024),
        purpose: CallPurpose::Conversation,
    }
}

fn transform_option() -> heycode_core::ProviderRequestOption {
    OpenRouterTransformPolicy::all_disabled()
        .provider_option(OpenRouterTransformRequestContext::new(true, false))
        .unwrap()
}

async fn run_search_routing(secret: &CredentialSecret) -> Result<(), FailureClass> {
    let evidence = Arc::new(Mutex::new(ClosedLaneEvidence::default()));
    let inner = ReqwestHttpTransport::new().map_err(|_| FailureClass::HostUnreachable)?;
    let routing = OpenRouterRoutingPolicy::new(
        Vec::new(),
        true,
        true,
        OpenRouterDataCollection::Allow,
        None,
    )
    .map_err(|_| FailureClass::AssertionFailed)?;
    let provider = OpenRouterProvider::from_key_with_transport_and_routing(
        secret.expose().to_owned(),
        Some(MODEL.to_owned()),
        HttpService::new(Arc::new(EvidenceTransport {
            inner,
            evidence: evidence.clone(),
        })),
        routing,
        vec![transform_option()],
    )
    .map_err(|_| FailureClass::ProtocolMismatch)?;
    let call = InferenceAdapter::resolve(&provider, draft(&provider), &model())
        .map_err(|_| FailureClass::AssertionFailed)?;
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    if let Some(error) = events.iter().find_map(|event| event.as_ref().err()) {
        return Err(failure_class(error.class()));
    }
    let events = events
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| failure_class(error.class()))?;
    let citation = events
        .iter()
        .any(|event| matches!(event, InferenceEvent::Citation { .. }));
    let usage = events.iter().any(|event| {
        matches!(
            event,
            InferenceEvent::ServerToolUsage(usage)
                if usage.logical() == "web_search"
                    && usage.requests() > 0
                    && usage.evidence() == ServerToolUsageEvidence::ProviderAggregate
        )
    });
    let replay_ready = events.iter().any(|event| {
        matches!(
            event,
            InferenceEvent::ProviderState(state)
                if state.kind() == ProviderStateKind::ChatAssistantMessage
                    && state
                        .data()
                        .get("annotations")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|annotations| !annotations.is_empty())
        )
    });
    let finished = events
        .iter()
        .any(|event| matches!(event, InferenceEvent::Finish(_)));
    let invented_call = events.iter().any(|event| {
        matches!(
            event,
            InferenceEvent::ServerToolCall { .. } | InferenceEvent::ServerToolResult { .. }
        )
    });
    let evidence = evidence.lock().unwrap();
    let exact_request = evidence.request_count == 1
        && evidence.request
            == (ClosedRequestEvidence {
                exact_model: true,
                explicit_routing: true,
                exact_web_policy: true,
                explicit_transform_policy: true,
            });
    let exact_response = evidence.citation_annotations > 0
        && evidence.aggregate_requests.is_some_and(|count| count > 0)
        && !evidence.duplicate_aggregate
        && !evidence.conflicting_aggregate_aliases;
    if citation
        && usage
        && replay_ready
        && finished
        && !invented_call
        && exact_request
        && exact_response
    {
        Ok(())
    } else {
        Err(FailureClass::AssertionFailed)
    }
}

const fn failure_class(class: ProviderErrorClass) -> FailureClass {
    match class {
        ProviderErrorClass::Authentication => FailureClass::Unauthorized,
        ProviderErrorClass::RateLimited | ProviderErrorClass::Overloaded => {
            FailureClass::RateLimited
        }
        ProviderErrorClass::Timeout => FailureClass::Timeout,
        ProviderErrorClass::Network => FailureClass::HostUnreachable,
        ProviderErrorClass::Protocol => FailureClass::ProtocolMismatch,
        ProviderErrorClass::Server
        | ProviderErrorClass::Overflow
        | ProviderErrorClass::ContextWindowExceeded
        | ProviderErrorClass::Conflict
        | ProviderErrorClass::InvalidRequest
        | ProviderErrorClass::Cancelled => FailureClass::Unclassified,
    }
}

fn artifact_directory() -> PathBuf {
    std::env::var_os("HEYCODE_LIVE_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/live-artifacts"))
}

fn artifact(
    outcome: LiveOutcome,
    recorded_at: u64,
    latency: Option<Duration>,
    notes: &[&str],
) -> LiveArtifact {
    ArtifactRecorder::withholding()
        .record(
            RouteId::new(OpenRouterProvider::NAME, MODEL).unwrap(),
            outcome,
            recorded_at,
            latency,
            None,
            notes,
        )
        .unwrap()
}

fn write_artifact(directory: &Path, name: &str, artifact: &LiveArtifact) {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(name);
    let bytes = serde_json::to_vec_pretty(artifact).unwrap();
    let mut output = atomic_write_file::AtomicWriteFile::open(&path).unwrap();
    output.write_all(&bytes).unwrap();
    output.commit().unwrap();
}

fn production_prerequisite_passed(json: &str, now: u64) -> bool {
    let Ok(artifact) = LiveArtifact::from_json(json) else {
        return false;
    };
    artifact.route().provider() == OpenRouterProvider::NAME
        && artifact.route().model() == MODEL
        && artifact.outcome().passed()
        && artifact.content().body().is_none()
        && artifact.is_fresh_at(now, FRESHNESS)
        && artifact
            .notes()
            .iter()
            .any(|note| note.as_str() == PRODUCTION_NOTE)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[test]
fn por07_request_observer_proves_policy_without_retaining_model_content() {
    let body = serde_json::to_vec(&serde_json::json!({
        "model":MODEL,
        "messages":[{"role":"user","content":"private-prompt-canary"}],
        "stream":true,
        "provider":{
            "allow_fallbacks":true,
            "require_parameters":true,
            "data_collection":"allow"
        },
        "tools":[{
            "type":"openrouter:web_search",
            "parameters":{
                "engine":"auto",
                "max_results":5,
                "max_uses":3,
                "max_total_results":15,
                "max_characters":4000
            }
        }],
        "max_tool_calls":5,
        "plugins":[
            {"id":"context-compression","enabled":false},
            {"id":"file-parser","enabled":false},
            {"id":"response-healing","enabled":false}
        ]
    }))
    .unwrap();

    let evidence = inspect_request(&body);
    assert_eq!(
        evidence,
        ClosedRequestEvidence {
            exact_model: true,
            explicit_routing: true,
            exact_web_policy: true,
            explicit_transform_policy: true,
        }
    );
    assert!(!format!("{evidence:?}").contains("private-prompt-canary"));
}

#[test]
fn por07_combined_artifact_requires_the_fresh_production_text_tool_proof() {
    let now = 1_788_123_354_000;
    let valid = artifact(
        LiveOutcome::Passed,
        now,
        Some(Duration::from_millis(1)),
        &[PRODUCTION_NOTE],
    );
    assert!(production_prerequisite_passed(
        &serde_json::to_string(&valid).unwrap(),
        now
    ));
    for invalid in [
        artifact(
            LiveOutcome::Passed,
            now.saturating_sub(u64::try_from(FRESHNESS.as_millis()).unwrap() + 1),
            None,
            &[PRODUCTION_NOTE],
        ),
        artifact(LiveOutcome::Passed, now + 1, None, &[PRODUCTION_NOTE]),
        artifact(
            LiveOutcome::Failed {
                class: FailureClass::AssertionFailed,
            },
            now,
            None,
            &[PRODUCTION_NOTE],
        ),
        artifact(LiveOutcome::Passed, now, None, &["search only"]),
    ] {
        assert!(!production_prerequisite_passed(
            &serde_json::to_string(&invalid).unwrap(),
            now
        ));
    }
}

#[tokio::test]
async fn live_glm_text_tool_search_routing_artifact_is_explicitly_gated() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let recorded_at = unix_time_ms();
    let directory = artifact_directory();
    let Some(key) = std::env::var(OpenRouterProvider::API_KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
    else {
        let skipped = artifact(
            LiveOutcome::Skipped {
                reason: SkipReason::NoCredential,
            },
            recorded_at,
            None,
            &[],
        );
        write_artifact(&directory, CONFORMANCE_ARTIFACT, &skipped);
        panic!("POR07 requires a process-scoped OpenRouter credential");
    };
    let prerequisite = std::fs::read_to_string(directory.join(PRODUCTION_ARTIFACT)).ok();
    if !prerequisite
        .as_deref()
        .is_some_and(|json| production_prerequisite_passed(json, recorded_at))
    {
        let skipped = artifact(
            LiveOutcome::Skipped {
                reason: SkipReason::PrerequisiteFailed,
            },
            recorded_at,
            None,
            &[],
        );
        write_artifact(&directory, CONFORMANCE_ARTIFACT, &skipped);
        panic!("POR07 requires the fresh production catalog/text/tool artifact");
    }

    let secret = CredentialSecret::new(key);
    let started = Instant::now();
    let outcome = match run_search_routing(&secret).await {
        Ok(()) => LiveOutcome::Passed,
        Err(class) => LiveOutcome::Failed { class },
    };
    let notes: &[&str] = if outcome.passed() {
        &[CONFORMANCE_NOTE]
    } else {
        &[]
    };
    let combined = artifact(outcome, recorded_at, Some(started.elapsed()), notes);
    write_artifact(&directory, CONFORMANCE_ARTIFACT, &combined);
    let round_trip = LiveArtifact::from_json(&serde_json::to_string(&combined).unwrap()).unwrap();
    assert!(
        round_trip.is_fresh_at(unix_time_ms(), FRESHNESS),
        "POR07 artifact is outside its freshness window"
    );
    assert!(outcome.passed(), "POR07 live search/routing lane failed");
}

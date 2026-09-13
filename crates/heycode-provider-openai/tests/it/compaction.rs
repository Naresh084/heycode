//! POA04 native `/responses/compact` checkpoint and continuation boundary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEvent, SseEventStream, TransportError,
};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceInput, InputModality, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, NativeCompactionError,
    NativeFeature, Provider, RequestDraft, RetrySafety,
};
use heycode_provider_openai::{
    OPENAI_GPT_5_6_SOL, OpenAiCompactionClient, OpenAiCompactionFault, OpenAiPromptCacheControl,
    OpenAiPromptCacheMode, OpenAiProvider,
};
use tokio_util::sync::CancellationToken;

const SECRET: &str = "sk-proj-COMPACTION-SECRET-CANARY";
const OPAQUE: &str = "gAAAAABpM0Yj-OPAQUE-COMPACTION-CANARY";

type CompactRequest = (String, Vec<(String, String)>, serde_json::Value);

#[derive(Default)]
struct RecordingTransport {
    buffered: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    compact_requests: Mutex<Vec<CompactRequest>>,
    response_requests: Mutex<Vec<serde_json::Value>>,
}

impl RecordingTransport {
    fn with_response(response: HttpResponse) -> Arc<Self> {
        Self::with_result(Ok(response))
    }

    fn with_result(response: Result<HttpResponse, TransportError>) -> Arc<Self> {
        Arc::new(Self {
            buffered: Mutex::new(VecDeque::from([response])),
            ..Self::default()
        })
    }
}

impl HttpTransport for RecordingTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        if cancellation.is_cancelled() {
            return Box::pin(async { Err(TransportError::Cancelled) });
        }
        let body = serde_json::from_slice(request.body().unwrap()).unwrap();
        self.compact_requests.lock().unwrap().push((
            request.url().to_owned(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body,
        ));
        let response = self
            .buffered
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra compact request");
        Box::pin(async move { response })
    }

    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.response_requests
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap()).unwrap());
        Box::pin(futures::stream::iter([Ok(SseEvent {
            event: "response.completed".to_owned(),
            data: serde_json::json!({
                "type":"response.completed","sequence_number":0,
                "response":{"id":"resp_after_compaction","status":"completed","output":[],
                    "usage":{
                        "input_tokens":5,
                        "input_tokens_details":{"cached_tokens":2,"cache_write_tokens":1},
                        "output_tokens":1,
                        "output_tokens_details":{"reasoning_tokens":0},
                        "total_tokens":6
                    }}
            })
            .to_string(),
            id: None,
            retry_ms: None,
        })]))
    }
}

fn response(body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status: 200,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: body.to_string().into_bytes(),
    }
}

fn checkpoint_response() -> serde_json::Value {
    serde_json::json!({
        "id":"resp_compact_1",
        "object":"response.compaction",
        "created_at":1_764_967_971_u64,
        "output":[
            {
                "id":"msg_000","type":"message","status":"completed","role":"user",
                "content":[{"type":"input_text","text":"continue the migration"}]
            },
            {
                "id":"cmp_001","type":"compaction","encrypted_content":OPAQUE,
                "provider_extension":{"must_survive":true}
            }
        ],
        "usage":{
            "input_tokens":139,
            "input_tokens_details":{"cached_tokens":11,"cache_write_tokens":17},
            "output_tokens":438,
            "output_tokens_details":{"reasoning_tokens":64},
            "total_tokens":577
        }
    })
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: OPENAI_GPT_5_6_SOL.to_owned(),
        display_name: OPENAI_GPT_5_6_SOL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            native_compaction: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn compaction_draft(inputs: Vec<InferenceInput>) -> RequestDraft {
    let mut draft = draft(inputs);
    draft.native_features = vec![NativeFeature::Compaction];
    draft.max_output_tokens = Some(8_192);
    draft.purpose = CallPurpose::Compaction;
    draft
}

fn draft(inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: "openai".to_owned(),
        model: OPENAI_GPT_5_6_SOL.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1_000,
        system: None,
        inputs,
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    }
}

#[tokio::test]
async fn opaque_checkpoint_is_durable_and_continues_unchanged() {
    let transport = RecordingTransport::with_response(response(checkpoint_response()));
    let http = HttpService::new(transport.clone());
    let client = OpenAiCompactionClient::new(http.clone(), SECRET).unwrap();
    let input = vec![serde_json::json!({
        "type":"message","role":"user",
        "content":[{"type":"input_text","text":"continue the migration"}]
    })];
    let checkpoint = client
        .compact(OPENAI_GPT_5_6_SOL, input.clone(), CancellationToken::new())
        .await
        .unwrap();

    {
        let requests = transport.compact_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "https://api.openai.com/v1/responses/compact");
        assert_eq!(
            requests[0].2,
            serde_json::json!({"model":OPENAI_GPT_5_6_SOL,"input":input})
        );
        assert!(requests[0].1.iter().any(|(name, value)| {
            name == "authorization" && value == &format!("Bearer {SECRET}")
        }));
    }

    assert_eq!(checkpoint.items().len(), 2);
    assert_eq!(
        checkpoint.compaction().data(),
        &checkpoint_response()["output"][1]
    );
    assert_eq!(checkpoint.cache_usage().cache_read_tokens(), 11);
    assert_eq!(checkpoint.cache_usage().cache_write_tokens(), 17);
    assert_eq!(
        checkpoint.cache_usage().activity(),
        heycode_provider_openai::OpenAiCacheActivity::ReadAndWrite
    );
    assert!(!format!("{checkpoint:?}").contains(OPAQUE));

    let durable = serde_json::to_vec(checkpoint.items()).unwrap();
    let restored: Vec<ProviderStateItem> = serde_json::from_slice(&durable).unwrap();
    assert_eq!(restored, checkpoint.items());
    assert!(restored.iter().all(|item| {
        item.provider() == "openai" && item.protocol() == ProviderProtocol::OpenAiResponses
    }));

    let appended = serde_json::json!({
        "type":"message","role":"user",
        "content":[{"type":"input_text","text":"next step"}]
    });
    let exact_continuation = checkpoint
        .continuation_input(vec![appended.clone()])
        .unwrap();
    assert_eq!(exact_continuation[0], checkpoint_response()["output"][0]);
    assert_eq!(exact_continuation[1], checkpoint_response()["output"][1]);
    assert_eq!(exact_continuation[2], appended);

    let provider = OpenAiProvider::new(http, SECRET, Some(OPENAI_GPT_5_6_SOL.to_owned())).unwrap();
    let mut continuation = restored
        .iter()
        .cloned()
        .map(InferenceInput::ProviderState)
        .collect::<Vec<_>>();
    continuation.push(InferenceInput::Message(ChatMessage::user("next step")));
    let call = Provider::inference_adapter(&provider)
        .unwrap()
        .resolve(draft(continuation), &model())
        .unwrap();
    let mut stream = Provider::inference_adapter(&provider).unwrap().stream(call);
    while let Some(event) = stream.next().await {
        event.unwrap();
    }

    let responses = transport.response_requests.lock().unwrap();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["input"][0], checkpoint_response()["output"][0]);
    assert_eq!(responses[0]["input"][1], checkpoint_response()["output"][1]);
    assert_eq!(responses[0]["input"][1]["encrypted_content"], OPAQUE);
    assert_eq!(
        responses[0]["input"][1]["provider_extension"]["must_survive"],
        true
    );
    assert_eq!(responses[0]["store"], false);
}

#[tokio::test]
async fn provider_native_adapter_posts_the_resolved_call_and_normalizes_usage() {
    let transport = RecordingTransport::with_response(response(checkpoint_response()));
    let provider = OpenAiProvider::new(
        HttpService::new(transport.clone()),
        SECRET,
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let compactor = adapter
        .native_compaction()
        .expect("the production OpenAI adapter must expose native compaction");
    let call = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user(
                "continue the migration",
            ))]),
            &model(),
        )
        .unwrap();

    assert_eq!(call.purpose(), CallPurpose::Compaction);
    assert_eq!(call.native_features(), [NativeFeature::Compaction]);
    assert_eq!(
        call.max_output_tokens(),
        None,
        "the compact endpoint has no max_output_tokens parameter"
    );

    let checkpoint = compactor
        .compact(call, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(checkpoint.items().len(), 2);
    assert_eq!(checkpoint.items()[1].data()["encrypted_content"], OPAQUE);
    assert_eq!(
        checkpoint.usage(),
        Some(heycode_core::TokenUsage {
            prompt_tokens: 139,
            completion_tokens: 438,
        })
    );

    let requests = transport.compact_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "https://api.openai.com/v1/responses/compact");
    assert_eq!(requests[0].2["model"], OPENAI_GPT_5_6_SOL);
    assert_eq!(
        requests[0].2["input"],
        serde_json::json!([{
            "type":"message","role":"user",
            "content":[{"type":"input_text","text":"continue the migration"}]
        }])
    );
    assert!(requests[0].2.get("stream").is_none());
    assert!(requests[0].2.get("store").is_none());
    assert!(requests[0].2.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn configured_prompt_cache_policy_reaches_the_normal_responses_body() {
    let transport = Arc::new(RecordingTransport::default());
    let provider = OpenAiProvider::new(
        HttpService::new(transport.clone()),
        SECRET,
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap()
    .with_prompt_caching(
        OpenAiPromptCacheControl::new("session-cache-01", OpenAiPromptCacheMode::Explicit).unwrap(),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let mut request = draft(vec![InferenceInput::Message(ChatMessage::user(
        "continue with the cached prefix",
    ))]);
    request.provider_options = Provider::request_options(&provider);
    let mut cache_model = model();
    cache_model.capabilities.prompt_cache = CapabilitySupport::Supported;

    let call = adapter.resolve(request, &cache_model).unwrap();
    assert_eq!(call.native_features(), [NativeFeature::PromptCache]);
    let mut stream = adapter.stream(call);
    while let Some(event) = stream.next().await {
        event.unwrap();
    }

    let requests = transport.response_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["prompt_cache_key"], "session-cache-01");
    assert_eq!(
        requests[0]["prompt_cache_options"],
        serde_json::json!({"mode":"explicit","ttl":"30m"})
    );
    assert_eq!(requests[0]["store"], false);
    assert!(requests[0].get("stream").is_some());
}

#[test]
fn a_hand_claimed_capability_cannot_enable_an_unproven_model() {
    let provider = OpenAiProvider::new(
        HttpService::new(Arc::new(RecordingTransport::default())),
        SECRET,
        Some("unlisted-model".to_owned()),
    )
    .unwrap();
    let mut model = model();
    model.id = "unlisted-model".to_owned();
    model.display_name = "unlisted-model".to_owned();
    let mut request = compaction_draft(vec![InferenceInput::Message(ChatMessage::user("compact"))]);
    request.model = "unlisted-model".to_owned();
    let error = Provider::inference_adapter(&provider)
        .unwrap()
        .resolve(request, &model)
        .unwrap_err();
    assert!(matches!(
        error,
        heycode_llm::ResolveError::Unproven {
            capability: heycode_llm::RequestedCapability::NativeCompaction,
            ..
        }
    ));
}

#[tokio::test]
async fn native_adapter_maps_cancellation_and_transport_without_body_text() {
    let canary = "provider-network-body-secret-canary";
    let transport = RecordingTransport::with_result(Err(TransportError::Network {
        message: canary.to_owned(),
    }));
    let provider = OpenAiProvider::new(
        HttpService::new(transport.clone()),
        SECRET,
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let compactor = adapter.native_compaction().unwrap();
    let call = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user("compact"))]),
            &model(),
        )
        .unwrap();
    let fault = compactor
        .compact(call, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(fault, NativeCompactionError::Transport);
    let rendered = format!("{fault:?} {fault}");
    assert!(!rendered.contains(canary));
    assert!(!rendered.contains(SECRET));

    let rejected_body = "provider-rejection-body-secret-canary";
    let transport = RecordingTransport::with_response(HttpResponse {
        status: 503,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: rejected_body.as_bytes().to_vec(),
    });
    let provider = OpenAiProvider::new(
        HttpService::new(transport),
        SECRET,
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user("compact"))]),
            &model(),
        )
        .unwrap();
    let fault = adapter
        .native_compaction()
        .unwrap()
        .compact(call, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(fault, NativeCompactionError::Rejected);
    assert!(!format!("{fault:?} {fault}").contains(rejected_body));

    let transport = RecordingTransport::with_response(response(checkpoint_response()));
    let provider = OpenAiProvider::new(
        HttpService::new(transport.clone()),
        SECRET,
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();
    let call = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user("compact"))]),
            &model(),
        )
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        adapter
            .native_compaction()
            .unwrap()
            .compact(call, cancellation)
            .await
            .unwrap_err(),
        NativeCompactionError::Cancelled
    );
    assert!(transport.compact_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unproven_models_and_malformed_checkpoints_fail_closed_without_io_or_echo() {
    let malformed = serde_json::json!({
        "id":"resp_compact_bad","object":"response.compaction","created_at":1,
        "output":[{"id":"cmp_bad","type":"compaction","encrypted_content":OPAQUE},
                  {"id":"msg_after","type":"message","role":"user","content":[]}],
        "usage":{}
    });
    let transport = RecordingTransport::with_response(response(malformed));
    let client = OpenAiCompactionClient::new(HttpService::new(transport.clone()), SECRET).unwrap();

    let fault = client
        .compact(
            "unlisted-model",
            vec![serde_json::json!({"type":"message","role":"user","content":[]} )],
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(fault, OpenAiCompactionFault::UnprovenCapability);
    assert!(transport.compact_requests.lock().unwrap().is_empty());

    let fault = client
        .compact(
            OPENAI_GPT_5_6_SOL,
            vec![serde_json::json!({"type":"message","role":"user","content":[]} )],
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(fault, OpenAiCompactionFault::InvalidResponse);
    let rendered = format!("{fault:?} {fault}");
    assert!(!rendered.contains(OPAQUE));
    assert!(!rendered.contains(SECRET));
}

#[tokio::test]
async fn cancellation_before_admission_sends_nothing() {
    let transport = RecordingTransport::with_response(response(checkpoint_response()));
    let client = OpenAiCompactionClient::new(HttpService::new(transport.clone()), SECRET).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        client
            .compact(
                OPENAI_GPT_5_6_SOL,
                vec![serde_json::json!({"type":"message","role":"user","content":[]} )],
                cancellation,
            )
            .await
            .unwrap_err(),
        OpenAiCompactionFault::Cancelled
    );
    assert!(transport.compact_requests.lock().unwrap().is_empty());
}

fn compacted_items() -> Vec<ProviderStateItem> {
    checkpoint_response()["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            ProviderStateItem::new(
                "openai",
                OPENAI_GPT_5_6_SOL,
                ProviderProtocol::OpenAiResponses,
                ProviderStateKind::ResponseOutputItem,
                item.clone(),
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn compacted_state_continuation_withholds_pre_output_replay() {
    let provider = OpenAiProvider::new(
        HttpService::new(Arc::new(RecordingTransport::default())),
        SECRET,
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let adapter = Provider::inference_adapter(&provider).unwrap();

    let mut inputs = compacted_items()
        .into_iter()
        .map(InferenceInput::ProviderState)
        .collect::<Vec<_>>();
    inputs.push(InferenceInput::Message(ChatMessage::user("next step")));
    let continuation = adapter.resolve(draft(inputs), &model()).unwrap();
    assert!(
        continuation.native_features().is_empty(),
        "the turn after a native compaction carries no native feature, so only the compaction \
         state item proves the request is not replayable"
    );
    assert_eq!(
        continuation.retry_spec().safety(),
        RetrySafety::Never,
        "a partially committed compacted turn must never be replayed pre-output"
    );

    let compaction = adapter
        .resolve(
            compaction_draft(vec![InferenceInput::Message(ChatMessage::user(
                "continue the migration",
            ))]),
            &model(),
        )
        .unwrap();
    assert_eq!(
        compaction.retry_spec().safety(),
        RetrySafety::Never,
        "the provider-executed compaction operation is not replayable either"
    );

    let plain = adapter
        .resolve(
            draft(vec![InferenceInput::Message(ChatMessage::user("hello"))]),
            &model(),
        )
        .unwrap();
    assert_eq!(
        plain.retry_spec().safety(),
        RetrySafety::StatelessPreOutput,
        "a bare conversation turn keeps the neutral replayable policy"
    );
}

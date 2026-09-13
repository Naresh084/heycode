//! PAWS04 Amazon Bedrock Converse provider profile.
//!
//! These cases run one altitude above P07's protocol conformance tests. P07
//! owns the wire shapes, the AWS event-stream framing and their edge cases;
//! nothing here re-tests them. What is proven here is the *composed* route:
//! that PAWS01's region, the PAWS01 credential reference, PAWS03's provider
//! identity and the caller's configured model together produce the request
//! AWS documents, and that the events come back through the whole profile.
//!
//! The event-stream frame builders below are a deliberate duplicate of P07's
//! test helpers. They live in that crate's test binary and are not importable;
//! a shared `heycode_llm::testing` fixture would be the fix, and that is a
//! `heycode-llm` change this row did not take.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_authorization_aws::{
    AWS_BEDROCK_API_KEY_REFERENCE, AwsProfileResolution, AwsRegion, AwsRegionResolution,
    ProcessAwsHost,
};
use heycode_credentials::CredentialSecret;
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream,
};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, ChatRequest, ChatToolCall, InferenceEvent,
    InferenceInput, InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ModelPerformance, ModelPricing, Provider, ProviderErrorClass, ProviderOptionContext,
    ProviderProtocol, RequestDraft, ToolSpec,
};
use heycode_provider_aws::{
    BEDROCK_PROVIDER, BedrockApiKeyAuthorizer, BedrockCachePlacement, BedrockCachePoint,
    BedrockCacheTtl, BedrockCatalog, BedrockConverseProvider, BedrockGuardrailConfig,
    BedrockGuardrailStreamMode, BedrockGuardrailTrace, BedrockPromptCacheCapabilities,
    BedrockPromptCacheConfig, BedrockRuntimeRequestMetadata, bedrock_converse_profile,
    converse_stream_eligible, runtime_url,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    ROTATED_TEST_SECRET, TEST_REGION, TEST_SECRET, process_credentials, query, query_for,
    rotating_credentials, summary,
};

const MODEL: &str = "anthropic.claude-fixture-v1:0";

/// Recognizable stand-in for a `ReasoningTextBlock.signature`.
///
/// It must survive exact provider state and replay while remaining absent from
/// ordinary `Debug` output.
const REASONING_SIGNATURE: &str = "SIGNATURE-THAT-MUST-SURVIVE-A-REPLAY";

// --------------------------------------------------------------------------
// AWS event-stream frame builders (see module note)
// --------------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFF_u32;
    for byte in bytes {
        state ^= u32::from(*byte);
        for _ in 0..8 {
            state = if state & 1 == 1 {
                0xEDB8_8320 ^ (state >> 1)
            } else {
                state >> 1
            };
        }
    }
    state ^ 0xFFFF_FFFF
}

fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(u8::try_from(name.len()).unwrap());
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let total = u32::try_from(16 + header_bytes.len() + payload.len()).unwrap();
    let mut message = Vec::new();
    message.extend_from_slice(&total.to_be_bytes());
    message.extend_from_slice(&u32::try_from(header_bytes.len()).unwrap().to_be_bytes());
    message.extend_from_slice(&crc32(&message).to_be_bytes());
    message.extend_from_slice(&header_bytes);
    message.extend_from_slice(payload);
    let crc = crc32(&message);
    message.extend_from_slice(&crc.to_be_bytes());
    message
}

fn event(event_type: &str, payload: serde_json::Value) -> Vec<u8> {
    frame(
        &[
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ],
        payload.to_string().as_bytes(),
    )
}

fn stream_body(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.iter().flatten().copied().collect()
}

fn text_stream() -> Vec<u8> {
    stream_body(&[
        event("messageStart", serde_json::json!({"role": "assistant"})),
        event(
            "contentBlockDelta",
            serde_json::json!({"contentBlockIndex": 0, "delta": {"text": "hi"}}),
        ),
        event(
            "contentBlockStop",
            serde_json::json!({"contentBlockIndex": 0}),
        ),
        event("messageStop", serde_json::json!({"stopReason": "end_turn"})),
        event(
            "metadata",
            serde_json::json!({
                "usage": {"inputTokens": 11, "outputTokens": 3, "totalTokens": 14},
                "metrics": {"latencyMs": 12}
            }),
        ),
    ])
}

fn tool_stream() -> Vec<u8> {
    stream_body(&[
        event("messageStart", serde_json::json!({"role": "assistant"})),
        event(
            "contentBlockStart",
            serde_json::json!({
                "contentBlockIndex": 0,
                "start": {"toolUse": {"toolUseId": "call-1", "name": "top_song"}}
            }),
        ),
        event(
            "contentBlockDelta",
            serde_json::json!({
                "contentBlockIndex": 0,
                "delta": {"toolUse": {"input": "{\"sign\":\"WZPZ\"}"}}
            }),
        ),
        event(
            "contentBlockStop",
            serde_json::json!({"contentBlockIndex": 0}),
        ),
        event("messageStop", serde_json::json!({"stopReason": "tool_use"})),
        event(
            "metadata",
            serde_json::json!({
                "usage": {"inputTokens": 11, "outputTokens": 3, "totalTokens": 14},
                "metrics": {"latencyMs": 12}
            }),
        ),
    ])
}

fn cache_stream() -> Vec<u8> {
    stream_body(&[
        event("messageStart", serde_json::json!({"role": "assistant"})),
        event(
            "contentBlockDelta",
            serde_json::json!({"contentBlockIndex": 0, "delta": {"text": "hi"}}),
        ),
        event(
            "contentBlockStop",
            serde_json::json!({"contentBlockIndex": 0}),
        ),
        event("messageStop", serde_json::json!({"stopReason": "end_turn"})),
        event(
            "metadata",
            serde_json::json!({
                "usage": {
                    "inputTokens": 11,
                    "outputTokens": 3,
                    "totalTokens": 14,
                    "cacheReadInputTokens": 1_000,
                    "cacheWriteInputTokens": 20
                },
                "metrics": {"latencyMs": 12}
            }),
        ),
    ])
}

fn cache_details_stream() -> Vec<u8> {
    stream_body(&[
        event("messageStart", serde_json::json!({"role": "assistant"})),
        event(
            "contentBlockDelta",
            serde_json::json!({"contentBlockIndex": 0, "delta": {"text": "hi"}}),
        ),
        event(
            "contentBlockStop",
            serde_json::json!({"contentBlockIndex": 0}),
        ),
        event("messageStop", serde_json::json!({"stopReason": "end_turn"})),
        event(
            "metadata",
            serde_json::json!({
                "usage": {
                    "inputTokens": 11,
                    "outputTokens": 3,
                    "totalTokens": 14,
                    "cacheReadInputTokens": 1_000,
                    "cacheWriteInputTokens": 20,
                    // Documented alongside the two counters: "The `cacheDetails`
                    // values tell you the ttl used for the number of token
                    // written to cache." It breaks `cacheWriteInputTokens` down
                    // per TTL rather than adding to it.
                    // https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html
                    "cacheDetails": [{"inputTokens": 20, "ttl": "1h"}]
                },
                "metrics": {"latencyMs": 12}
            }),
        ),
    ])
}

/// A thinking turn: reasoning text, then the signature that authenticates it,
/// then the visible answer.
///
/// `ReasoningTextBlock.signature` is "A token that verifies that the reasoning
/// text was generated by the model. If you pass a reasoning block back to the
/// API in a multi-turn conversation, include the text and its signature
/// unmodified."
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ReasoningTextBlock.html>
fn reasoning_stream() -> Vec<u8> {
    stream_body(&[
        event("messageStart", serde_json::json!({"role": "assistant"})),
        event(
            "contentBlockDelta",
            serde_json::json!({
                "contentBlockIndex": 0,
                "delta": {"reasoningContent": {"text": "weighing the options"}}
            }),
        ),
        event(
            "contentBlockDelta",
            serde_json::json!({
                "contentBlockIndex": 0,
                "delta": {"reasoningContent": {"signature": REASONING_SIGNATURE}}
            }),
        ),
        event(
            "contentBlockStop",
            serde_json::json!({"contentBlockIndex": 0}),
        ),
        event(
            "contentBlockDelta",
            serde_json::json!({"contentBlockIndex": 1, "delta": {"text": "hi"}}),
        ),
        event(
            "contentBlockStop",
            serde_json::json!({"contentBlockIndex": 1}),
        ),
        event("messageStop", serde_json::json!({"stopReason": "end_turn"})),
        event(
            "metadata",
            serde_json::json!({
                "usage": {"inputTokens": 11, "outputTokens": 3, "totalTokens": 14},
                "metrics": {"latencyMs": 12}
            }),
        ),
    ])
}

// --------------------------------------------------------------------------
// Injected transport
// --------------------------------------------------------------------------

type Recorded = Arc<Mutex<Vec<(String, Vec<(String, String)>, Vec<u8>)>>>;

struct StreamTransport {
    bodies: Mutex<Vec<Vec<u8>>>,
    requests: Recorded,
}

impl HttpTransport for StreamTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.requests.lock().unwrap().push((
            request.url().to_owned(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            request.body().unwrap_or_default().to_vec(),
        ));
        let mut bodies = self.bodies.lock().unwrap();
        let body = if bodies.is_empty() {
            Vec::new()
        } else {
            bodies.remove(0)
        };
        Box::pin(async move {
            Ok(HttpResponse {
                headers: std::collections::BTreeMap::new(),
                status: 200,
                content_type: Some("application/vnd.amazon.eventstream".to_owned()),
                body,
            })
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn region() -> AwsRegion {
    AwsRegion::new(TEST_REGION).unwrap()
}

fn provider_over(body: Vec<u8>) -> (BedrockConverseProvider, Recorded) {
    let requests: Recorded = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(StreamTransport {
        bodies: Mutex::new(vec![body]),
        requests: requests.clone(),
    }));
    let provider =
        BedrockConverseProvider::new(http, &region(), &CredentialSecret::new(TEST_SECRET), MODEL)
            .unwrap();
    (provider, requests)
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: MODEL.to_owned(),
        display_name: MODEL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(200_000),
        max_output_tokens: Some(8_192),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        reasoning: None,
    }
}

fn cache_model() -> ModelDescriptor {
    let mut descriptor = model();
    descriptor.capabilities.prompt_cache = CapabilitySupport::Supported;
    descriptor
}

fn runtime_metadata() -> BedrockRuntimeRequestMetadata {
    let capabilities = BedrockPromptCacheCapabilities::new(
        MODEL,
        vec![
            BedrockCachePlacement::Tools,
            BedrockCachePlacement::System,
            BedrockCachePlacement::LatestUserMessage,
        ],
        3,
        CapabilitySupport::Supported,
    )
    .unwrap();
    let cache = BedrockPromptCacheConfig::new(
        vec![
            BedrockCachePoint::new(BedrockCachePlacement::Tools, BedrockCacheTtl::OneHour),
            BedrockCachePoint::new(
                BedrockCachePlacement::System,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
            BedrockCachePoint::new(
                BedrockCachePlacement::LatestUserMessage,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
        ],
        capabilities,
    )
    .unwrap();
    let guardrail = BedrockGuardrailConfig::new("grabc123", "7")
        .unwrap()
        .with_trace(BedrockGuardrailTrace::Enabled)
        .with_stream_mode(BedrockGuardrailStreamMode::Sync);
    BedrockRuntimeRequestMetadata::new(Some(cache), Some(guardrail))
}

fn draft(tools: Vec<ToolSpec>) -> RequestDraft {
    RequestDraft {
        provider: BEDROCK_PROVIDER.to_owned(),
        model: MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: Some("Follow the repository law.".to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools,
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

fn tool() -> ToolSpec {
    ToolSpec {
        name: "top_song".to_owned(),
        description: "Get the most popular song played on a radio station.".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"sign": {"type": "string"}},
            "required": ["sign"],
        }),
    }
}

async fn events(provider: &BedrockConverseProvider, tools: Vec<ToolSpec>) -> Vec<InferenceEvent> {
    events_for(provider, draft(tools)).await
}

async fn events_for(
    provider: &BedrockConverseProvider,
    draft: RequestDraft,
) -> Vec<InferenceEvent> {
    let adapter = provider.inference_adapter().expect("adapter is advertised");
    let call = adapter.resolve(draft, &model()).unwrap();
    adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

/// The single JSON body the composed route put on the wire.
fn sent_body(requests: &Recorded) -> serde_json::Value {
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1, "exactly one request per turn");
    serde_json::from_slice(&recorded[0].2).expect("request body is JSON")
}

/// The second turn of a tool loop: the model asked for `top_song`, heycode ran it,
/// and the result goes back for the model to read.
fn continuation_draft(result_is_error: bool) -> RequestDraft {
    RequestDraft {
        inputs: vec![
            InferenceInput::Message(ChatMessage::user("what is the top song on WZPZ?")),
            InferenceInput::Message(ChatMessage::assistant_with_tool_calls(
                String::new(),
                vec![ChatToolCall {
                    id: "call-1".to_owned(),
                    name: "top_song".to_owned(),
                    arguments: "{\"sign\":\"WZPZ\"}".to_owned(),
                }],
            )),
            InferenceInput::Message(ChatMessage::tool_result(
                "call-1",
                "Elemental Hotel by Best Kept Secret",
                result_is_error,
            )),
        ],
        ..draft(vec![tool()])
    }
}

// --------------------------------------------------------------------------
// Cases
// --------------------------------------------------------------------------

#[test]
fn the_profile_binds_bedrock_identity_the_configured_model_and_the_paws01_credential() {
    let profile = bedrock_converse_profile("anthropic.some-model-v1:0");
    assert_eq!(profile.registry_name, BEDROCK_PROVIDER);
    assert_eq!(
        profile.descriptor,
        heycode_provider_aws::provider_descriptor(),
        "the catalog and the inference route must share one identity"
    );
    assert_eq!(
        profile.descriptor.protocols,
        vec![ProviderProtocol::BedrockConverse]
    );
    assert_eq!(profile.default_model, "anthropic.some-model-v1:0");
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(AWS_BEDROCK_API_KEY_REFERENCE)
    );
}

#[test]
fn converse_profile_reports_the_exact_custom_operation_credential_reference() {
    let custom = query_for("bedrock-production-key");
    let (context, service, _) = rotating_credentials(Some(TEST_SECRET));
    let provider = BedrockConverseProvider::from_credentials(
        HttpService::new(Arc::new(StreamTransport {
            bodies: Mutex::new(vec![text_stream()]),
            requests: Arc::new(Mutex::new(Vec::new())),
        })),
        &service,
        &custom,
        &region(),
        MODEL,
    )
    .unwrap();
    assert_eq!(
        provider.credential_reference(),
        Some("bedrock-production-key")
    );
    drop(context);
}

#[test]
fn no_model_default_is_invented_by_this_crate() {
    // The caller's `[llm] model` is the only source. Two different callers get
    // two different defaults from the same crate, which is the property that
    // stops a region-locked or uncitable id being baked in.
    assert_eq!(
        bedrock_converse_profile("us.anthropic.claude-sonnet-5").default_model,
        "us.anthropic.claude-sonnet-5"
    );
    assert_eq!(
        bedrock_converse_profile("amazon.nova-pro-v1:0").default_model,
        "amazon.nova-pro-v1:0"
    );
}

#[test]
fn the_route_targets_the_runtime_host_not_the_control_plane_or_mantle() {
    let url = runtime_url(&region());
    assert_eq!(url, "https://bedrock-runtime.us-east-1.amazonaws.com");
    assert!(!url.contains("bedrock-mantle"), "{url}");
    assert_ne!(url, "https://bedrock.us-east-1.amazonaws.com");
}

#[tokio::test]
async fn a_converse_request_carries_the_documented_path_and_the_bedrock_bearer_key() {
    let (provider, requests) = provider_over(text_stream());
    events(&provider, Vec::new()).await;
    let recorded = requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    let (url, headers, _) = &recorded[0];
    // A Bedrock version suffix contains a colon, and the URL layer
    // percent-encodes it in the path segment — `anthropic.…-v1:0` goes out as
    // `anthropic.…-v1%3A0`. RFC 3986 permits both spellings and the AWS SDKs
    // encode it too, so this is the correct wire form and not something to
    // "fix" later. P07's fixture uses a colon-free id, so nothing pinned this
    // until now.
    assert_eq!(
        url,
        "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-fixture-v1%3A0/converse-stream",
        "the documented ConverseStream path is POST /model/{{modelId}}/converse-stream"
    );
    assert!(
        headers.contains(&("authorization".to_owned(), format!("Bearer {TEST_SECRET}"))),
        "the PAWS01 Bedrock API key is a bearer token here: {headers:?}"
    );
}

#[tokio::test]
async fn a_text_stream_reaches_the_caller_through_the_composed_profile() {
    let (provider, _) = provider_over(text_stream());
    let events = events(&provider, Vec::new()).await;
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            InferenceEvent::TextDelta(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hi");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::Finish(_))),
        "the stream must settle"
    );
}

#[tokio::test]
async fn a_tool_call_reaches_the_caller_through_the_composed_profile() {
    let (provider, requests) = provider_over(tool_stream());
    let events = events(&provider, vec![tool()]).await;
    let body: serde_json::Value =
        serde_json::from_slice(&requests.lock().unwrap()[0].2).expect("request body is JSON");
    assert!(
        body.get("toolConfig").is_some(),
        "a draft carrying tools must reach the wire as toolConfig: {body}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ToolCallDelta { .. })),
        "the composed route must surface the tool call: {events:?}"
    );
}

#[tokio::test]
async fn reported_cache_counters_reach_prompt_usage_through_the_composed_profile() {
    // Response-side accounting for an unconfigured provider instance. Nothing
    // here requests a cache read; the configured PAWS06 path is covered by
    // `cache_guardrail_route_and_exact_usage_cross_one_provider_turn`.
    let (provider, _) = provider_over(cache_stream());
    let events = events(&provider, Vec::new()).await;
    let usage = events
        .iter()
        .find_map(|event| match event {
            InferenceEvent::Usage(usage) => Some(*usage),
            _ => None,
        })
        .expect("terminal usage is published");
    assert_eq!(
        usage.prompt_tokens,
        11 + 1_000 + 20,
        "documented total input = inputTokens + cacheRead + cacheWrite"
    );
}

#[tokio::test]
async fn the_legacy_chat_path_fails_loud_rather_than_degrading() {
    let (provider, requests) = provider_over(text_stream());
    let mut stream = provider.stream(ChatRequest {
        model: MODEL.to_owned(),
        messages: vec![ChatMessage::user("hello")],
        tools: None,
        temperature: None,
        max_tokens: None,
    });
    let first = stream.next().await.expect("one item");
    let error = first.expect_err("the legacy path must not succeed");
    assert_eq!(error.class(), ProviderErrorClass::InvalidRequest);
    assert!(
        requests.lock().unwrap().is_empty(),
        "a refused legacy call must not reach AWS"
    );
}

#[tokio::test]
async fn converse_streaming_eligibility_requires_explicit_streaming_evidence() {
    for (published, eligible) in [
        (Some(true), true),
        (Some(false), false),
        // No `responseStreamingSupported` field: the model may or may not
        // stream, and this adapter only streams.
        (None, false),
    ] {
        let row = summary_with_streaming(published).await;
        assert_eq!(
            converse_stream_eligible(&row),
            eligible,
            "responseStreamingSupported = {published:?}"
        );
    }
}

#[tokio::test]
async fn a_missing_bedrock_credential_fails_the_operation_before_a_request_exists() {
    let (context, service, _) = rotating_credentials(None);
    let requests: Recorded = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(StreamTransport {
        bodies: Mutex::new(vec![text_stream()]),
        requests: requests.clone(),
    }));
    let provider =
        BedrockConverseProvider::from_credentials(http, &service, &query(), &region(), MODEL)
            .expect("credential presence is an operation-time fact");
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter.resolve(draft(Vec::new()), &model()).unwrap();
    let events: Vec<_> = adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect()
        .await;
    assert!(events.iter().any(Result::is_err), "{events:?}");
    assert!(requests.lock().unwrap().is_empty());
    drop(context);
}

#[tokio::test]
async fn converse_resolves_the_exact_bedrock_key_once_per_operation() {
    let (context, service, secret) = rotating_credentials(Some(TEST_SECRET));
    let requests: Recorded = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(StreamTransport {
        bodies: Mutex::new(vec![text_stream(), text_stream()]),
        requests: requests.clone(),
    }));
    let provider =
        BedrockConverseProvider::from_credentials(http, &service, &query(), &region(), MODEL)
            .unwrap();
    assert!(matches!(
        provider
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(_)
    ));

    events(&provider, Vec::new()).await;
    secret.set(Some(ROTATED_TEST_SECRET));
    events(&provider, Vec::new()).await;

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].1.iter().any(|(name, value)| {
        name == "authorization" && value == &format!("Bearer {TEST_SECRET}")
    }));
    assert!(requests[1].1.iter().any(|(name, value)| {
        name == "authorization" && value == &format!("Bearer {ROTATED_TEST_SECRET}")
    }));
    drop(context);
}

#[tokio::test]
async fn a_tool_result_turn_reaches_the_wire_as_the_documented_tool_use_and_tool_result_blocks() {
    // The first turn is what `a_tool_call_reaches_the_caller…` covers. This is
    // the turn that follows it, and it is the one an agent loop lives or dies
    // on: Converse requires the assistant `toolUse` block to be echoed back and
    // answered by a `toolResult` block in the immediately following user turn.
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolUseBlock.html
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolResultBlock.html
    let (provider, requests) = provider_over(text_stream());
    events_for(&provider, continuation_draft(false)).await;
    let body = sent_body(&requests);
    assert_eq!(
        body["messages"],
        serde_json::json!([
            {"role": "user", "content": [{"text": "what is the top song on WZPZ?"}]},
            {
                "role": "assistant",
                "content": [{
                    "toolUse": {
                        "toolUseId": "call-1",
                        "name": "top_song",
                        "input": {"sign": "WZPZ"}
                    }
                }]
            },
            {
                "role": "user",
                "content": [{
                    "toolResult": {
                        "toolUseId": "call-1",
                        "content": [{"text": "Elemental Hotel by Best Kept Secret"}]
                    }
                }]
            }
        ]),
        "unexpected continuation transcript: {body}"
    );
    // `SystemContentBlock` is its own top-level array, not a message.
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_SystemContentBlock.html
    assert_eq!(
        body["system"],
        serde_json::json!([{"text": "Follow the repository law."}])
    );
    assert_eq!(
        body["toolConfig"],
        serde_json::json!({
            "tools": [{
                "toolSpec": {
                    "name": "top_song",
                    "description": "Get the most popular song played on a radio station.",
                    "inputSchema": {"json": {
                        "type": "object",
                        "properties": {"sign": {"type": "string"}},
                        "required": ["sign"],
                    }}
                }
            }],
            "toolChoice": {"auto": {}}
        }),
        "unexpected toolConfig: {body}"
    );
}

#[tokio::test]
async fn a_failed_tool_result_is_reported_to_the_model_and_a_successful_one_stays_implicit() {
    // `ToolResultBlock.status` is `success | error` and is documented as
    // supported only by some model families, so a success is left unsaid and
    // only a real failure is stated.
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolResultBlock.html
    let (provider, requests) = provider_over(text_stream());
    events_for(&provider, continuation_draft(true)).await;
    let failed = sent_body(&requests);
    assert_eq!(
        failed["messages"][2]["content"][0]["toolResult"]["status"],
        serde_json::json!("error"),
        "a denied or failed tool must not read as a success: {failed}"
    );

    let (provider, requests) = provider_over(text_stream());
    events_for(&provider, continuation_draft(false)).await;
    let succeeded = sent_body(&requests);
    assert!(
        succeeded["messages"][2]["content"][0]["toolResult"]
            .get("status")
            .is_none(),
        "a successful tool result states no status: {succeeded}"
    );
}

#[tokio::test]
async fn a_per_ttl_cache_breakdown_is_not_added_a_second_time_to_the_input_total() {
    // `cacheDetails` reports the TTL used for tokens already counted by
    // `cacheWriteInputTokens`, so summing it would bill the same 20 tokens
    // twice. The documented total is exactly the three-term formula.
    // https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html
    let (provider, _) = provider_over(cache_details_stream());
    let events = events_for(&provider, draft(Vec::new())).await;
    let usage = events
        .iter()
        .find_map(|event| match event {
            InferenceEvent::Usage(usage) => Some(*usage),
            _ => None,
        })
        .expect("terminal usage is published");
    assert_eq!(usage.prompt_tokens, 11 + 1_000 + 20, "{usage:?}");
}

#[tokio::test]
async fn an_unconfigured_profile_requests_no_explicit_cache_checkpoint() {
    // Prompt caching is explicit provider policy. Omitting PAWS06 metadata
    // must never inherit an account, model or crate default.
    // https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html
    let (provider, requests) = provider_over(text_stream());
    events_for(&provider, continuation_draft(false)).await;
    let body = sent_body(&requests);
    assert!(
        !body.to_string().contains("cachePoint"),
        "unexpected cache checkpoint: {body}"
    );
}

#[tokio::test]
async fn cache_guardrail_route_and_exact_usage_cross_one_provider_turn() {
    let requests: Recorded = Arc::new(Mutex::new(Vec::new()));
    let provider = BedrockConverseProvider::new(
        HttpService::new(Arc::new(StreamTransport {
            bodies: Mutex::new(vec![cache_details_stream()]),
            requests: requests.clone(),
        })),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        MODEL,
    )
    .unwrap()
    .with_runtime_metadata(runtime_metadata());
    let selected = cache_model();
    let mut request = continuation_draft(false);
    request.provider_options = provider
        .request_options_for(ProviderOptionContext::new(&selected, &[]))
        .unwrap();
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter.resolve(request, &selected).unwrap();
    let events = adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();

    let body = sent_body(&requests);
    assert_eq!(
        body["toolConfig"]["tools"].as_array().unwrap().last(),
        Some(&serde_json::json!({"cachePoint":{"type":"default","ttl":"1h"}}))
    );
    assert_eq!(
        body["system"].as_array().unwrap().last(),
        Some(&serde_json::json!({"cachePoint":{"type":"default"}}))
    );
    assert_eq!(
        body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .last(),
        Some(&serde_json::json!({"cachePoint":{"type":"default"}}))
    );
    assert_eq!(
        body["guardrailConfig"],
        serde_json::json!({
            "guardrailIdentifier":"grabc123",
            "guardrailVersion":"7",
            "trace":"enabled",
            "streamProcessingMode":"sync"
        })
    );

    let metadata_index = events
        .iter()
        .position(|event| matches!(event, InferenceEvent::ResponseMetadata(_)))
        .expect("detailed cache facts must publish");
    let usage_index = events
        .iter()
        .position(|event| matches!(event, InferenceEvent::Usage(_)))
        .expect("aggregate usage must publish");
    let finish_index = events
        .iter()
        .position(|event| matches!(event, InferenceEvent::Finish(_)))
        .expect("the stream must settle");
    assert!(metadata_index < usage_index && usage_index < finish_index);
    let detailed = events.iter().find_map(|event| match event {
        InferenceEvent::ResponseMetadata(metadata) => metadata.cache_usage(),
        _ => None,
    });
    let detailed = detailed.expect("cache usage must remain exact");
    assert_eq!(detailed.input_tokens(), 1_031);
    assert_eq!(detailed.output_tokens(), 3);
    assert_eq!(detailed.uncached_input_tokens(), Some(11));
    assert_eq!(detailed.cache_read_tokens(), 1_000);
    assert_eq!(detailed.cache_write_tokens(), 20);
    assert_eq!(detailed.cache_write_5m_tokens(), Some(0));
    assert_eq!(detailed.cache_write_1h_tokens(), Some(20));
}

#[tokio::test]
async fn reasoning_signature_survives_exact_state_and_replay_but_not_debug() {
    // AWS: if you pass a reasoning block back in a multi-turn conversation,
    // "include the text and its signature unmodified".
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ReasoningTextBlock.html
    let (provider, _) = provider_over(reasoning_stream());
    let events = events_for(&provider, draft(Vec::new())).await;
    let state = events
        .iter()
        .find_map(|event| match event {
            InferenceEvent::ProviderState(state) => Some(state.clone()),
            _ => None,
        })
        .expect("terminal success must publish complete Converse state");
    assert_eq!(
        state.data(),
        &serde_json::json!({
            "role":"assistant",
            "content":[
                {"reasoningContent":{
                    "text":"weighing the options",
                    "signature":REASONING_SIGNATURE
                }},
                {"text":"hi"}
            ]
        })
    );
    assert!(!format!("{state:?}").contains(REASONING_SIGNATURE));

    let (continuation, requests) = provider_over(text_stream());
    let mut request = draft(Vec::new());
    request.inputs = vec![
        InferenceInput::Message(ChatMessage::user("first question")),
        InferenceInput::ProviderState(state),
        InferenceInput::Message(ChatMessage::user("follow-up")),
    ];
    events_for(&continuation, request).await;
    let body = sent_body(&requests);
    assert_eq!(
        body["messages"][1]["content"][0]["reasoningContent"]["signature"],
        serde_json::json!(REASONING_SIGNATURE)
    );
    assert_eq!(body["messages"][1]["content"][1]["text"], "hi");
}

/// Everything a live turn needs, or nothing.
///
/// AGENTS.md §9: real-network tests are gated behind `HEYCODE_E2E=1` and skip
/// silently without keys. Each `else { return None }` below is one documented
/// reason a run is a skip rather than a pass, so the skip is auditable by
/// reading it.
///
/// `HEYCODE_E2E_BEDROCK_TOOLS=1` is the operator stating that the model they named
/// is tool-capable. This test cannot discover that — `ListFoundationModels`
/// publishes no tool-use field — and refuses to assume it, so without the flag
/// the live turn advertises no tools.
fn live_smoke_environment() -> Option<(AwsRegion, String, String, bool)> {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return None;
    }
    std::env::var_os("AWS_BEARER_TOKEN_BEDROCK").filter(|value| !value.is_empty())?;
    let host = ProcessAwsHost;
    let profile = AwsProfileResolution::resolve(&host);
    let region = AwsRegionResolution::resolve(&host, &profile)
        .region()
        .cloned()?;
    let model = std::env::var("HEYCODE_E2E_BEDROCK_MODEL").ok()?;
    let foundation_model = std::env::var("HEYCODE_E2E_BEDROCK_FOUNDATION_MODEL").ok()?;
    let tools = std::env::var("HEYCODE_E2E_BEDROCK_TOOLS").ok().as_deref() == Some("1");
    Some((region, model, foundation_model, tools))
}

/// Real-network smoke over the same three surfaces the fixtures cover: the
/// stream settles, an advertised tool is never answered by a different one, and
/// terminal usage — the carrier of the cache counters — is published.
///
/// **Never observed green.** This host has none of the required process-scoped
/// Bedrock key/region/model inputs, so the only branch observed here is the
/// skip. Everything asserted below is what *would* be checked, not what has
/// been checked.
///
/// ```text
/// HEYCODE_E2E=1 AWS_REGION=us-east-1 AWS_BEARER_TOKEN_BEDROCK=… \
///   HEYCODE_E2E_BEDROCK_MODEL=us.anthropic.claude-sonnet-5 \
///   HEYCODE_E2E_BEDROCK_FOUNDATION_MODEL=anthropic.claude-sonnet-5 \
///   HEYCODE_E2E_BEDROCK_TOOLS=1 \
///   cargo test -p heycode-provider-aws live_converse_smoke
/// ```
#[tokio::test]
async fn live_converse_smoke() {
    let Some((region, model, foundation_model, tools_declared)) = live_smoke_environment() else {
        return;
    };
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let http = HttpService::new(Arc::new(transport));
    let (credential_context, credentials) = process_credentials();
    let catalog = BedrockCatalog::new(
        http.clone(),
        Arc::new(BedrockApiKeyAuthorizer::new(credentials.clone(), query())),
        &region,
    );
    let discovered = catalog
        .discover(CancellationToken::new())
        .await
        .expect("the live Bedrock control-plane catalog must load");
    let streaming_evidence = discovered
        .iter()
        .find(|row| row.descriptor().id == foundation_model)
        .expect("the configured foundation model must be visible in this region");
    assert!(
        converse_stream_eligible(streaming_evidence),
        "the configured foundation model must explicitly support streaming"
    );
    let provider = BedrockConverseProvider::from_credentials(
        http,
        &credentials,
        &query(),
        &region,
        model.clone(),
    )
    .unwrap();
    let adapter = provider.inference_adapter().unwrap();

    let mut descriptor = model_for(&model);
    descriptor.capabilities.tools = if tools_declared {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    };
    let mut draft = draft_for(&model);
    if tools_declared {
        draft.tools = vec![tool()];
        draft.inputs = vec![InferenceInput::Message(ChatMessage::user(
            "what is the top song on WZPZ?",
        ))];
    }

    let call = adapter.resolve(draft, &descriptor).expect("resolution");
    let events: Vec<_> = adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect()
        .await;
    let failures: Vec<_> = events
        .iter()
        .filter_map(|event| event.as_ref().err())
        .collect();
    assert!(
        failures.is_empty(),
        "a live Converse turn failed: {failures:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish { .. }))),
        "a live Converse turn must settle"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Usage(_)))),
        "terminal usage carries the cache counters and must be published"
    );
    for event in &events {
        if let Ok(InferenceEvent::ToolCallDelta {
            name: Some(name), ..
        }) = event
        {
            assert_eq!(name, "top_song", "an unadvertised tool was requested");
        }
    }
    drop(credential_context);
}

fn model_for(id: &str) -> ModelDescriptor {
    ModelDescriptor {
        id: id.to_owned(),
        ..model()
    }
}

fn draft_for(id: &str) -> RequestDraft {
    RequestDraft {
        model: id.to_owned(),
        ..draft(Vec::new())
    }
}

async fn summary_with_streaming(
    streaming: Option<bool>,
) -> heycode_provider_aws::BedrockFoundationModel {
    let mut row = summary(MODEL);
    match streaming {
        Some(value) => {
            row["responseStreamingSupported"] = serde_json::Value::Bool(value);
        }
        None => {
            row.as_object_mut()
                .unwrap()
                .remove("responseStreamingSupported");
        }
    }
    super::support::normalize_one(row).await
}

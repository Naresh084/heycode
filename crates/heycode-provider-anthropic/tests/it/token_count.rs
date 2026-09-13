//! PAN01 provider-measured token counting through `POST /v1/messages/count_tokens`.
//!
//! Fixtures mirror the documented response shape: a single object carrying
//! `input_tokens`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream,
};
use heycode_llm::{Role, TokenCountFailureKind, TokenCountRequest, TokenCounter, TokenEvidence};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_TOKEN_COUNTER_ID, ANTHROPIC_VERSION,
    AnthropicContextEditingPolicy, AnthropicThinkingClear, AnthropicThinkingKeep,
    AnthropicTokenCounter, AnthropicTokenCounterConfig,
};
use tokio_util::sync::CancellationToken;

use super::catalog::{credentials, http, query, response};

const TEST_SECRET: &str = "sk-ant-test-not-a-real-key";

fn counter(
    secret: Option<&str>,
    responses: Vec<heycode_http::HttpResponse>,
) -> (
    heycode_core::Context,
    AnthropicTokenCounter,
    super::catalog::RecordedRequests,
) {
    let (context, credentials) = credentials(secret);
    let (service, requests) = http(responses);
    let counter = AnthropicTokenCounter::new(
        service,
        (*credentials).clone(),
        AnthropicTokenCounterConfig::official(query()),
    )
    .unwrap();
    (context, counter, requests)
}

fn message(role: Role, content: &str) -> heycode_llm::ChatMessage {
    let mut message = heycode_llm::ChatMessage::user(content);
    message.role = role;
    message
}

struct BodyRecordingTransport {
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    responses: Mutex<Vec<HttpResponse>>,
}

impl HttpTransport for BodyRecordingTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().expect("count request body")).unwrap());
        let response = self.responses.lock().unwrap().remove(0);
        Box::pin(async move { Ok(response) })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn structured_counter(
    response_count: usize,
) -> (
    heycode_core::Context,
    AnthropicTokenCounter,
    Arc<Mutex<Vec<serde_json::Value>>>,
) {
    let (context, credentials) = credentials(Some(TEST_SECRET));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let responses = (0..response_count)
        .map(|_| HttpResponse {
            status: 200,
            content_type: Some("application/json".to_owned()),
            headers: BTreeMap::new(),
            body: serde_json::json!({"input_tokens":42})
                .to_string()
                .into_bytes(),
        })
        .collect();
    let counter = AnthropicTokenCounter::new(
        HttpService::new(Arc::new(BodyRecordingTransport {
            bodies: bodies.clone(),
            responses: Mutex::new(responses),
        })),
        (*credentials).clone(),
        AnthropicTokenCounterConfig::official(query()),
    )
    .unwrap();
    (context, counter, bodies)
}

#[test]
fn the_counter_retains_the_current_provider_endpoint_evidence_class() {
    let (_context, counter, _requests) = counter(Some(TEST_SECRET), Vec::new());
    let descriptor = counter.descriptor();
    assert_eq!(descriptor.id().as_str(), ANTHROPIC_TOKEN_COUNTER_ID);
    assert_eq!(
        descriptor.evidence(),
        TokenEvidence::Estimated(heycode_llm::EstimationMethod::ProviderTokenizer)
    );
    assert!(!descriptor.evidence().is_exact());
    assert!(
        descriptor
            .scope()
            .serves("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
    );
    assert!(
        !descriptor
            .scope()
            .serves("openrouter", "z-ai/glm-5.3-flash"),
        "an Anthropic counter must not claim another provider's models"
    );
}

#[tokio::test]
async fn a_measured_count_sends_the_exact_documented_request() {
    let (_context, counter, requests) = counter(
        Some(TEST_SECRET),
        vec![response(200, serde_json::json!({"input_tokens": 2095}))],
    );
    let system = message(Role::System, "be terse");
    let user = message(Role::User, "count me");
    let tool = heycode_core::ToolSpec {
        name: "read".to_owned(),
        description: "read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    };
    let request = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&system)
        .with_message(&user)
        .with_tool(&tool);

    let tokens = counter
        .count(&request, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(tokens, 2095, "the provider's measurement is used verbatim");

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let (url, headers) = &recorded[0];
    assert_eq!(url, "https://api.anthropic.com/v1/messages/count_tokens");
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("anthropic-version"), Some(ANTHROPIC_VERSION));
    assert_eq!(header("x-api-key"), Some(TEST_SECRET));
    assert_eq!(header("content-type"), Some("application/json"));
}

#[tokio::test]
async fn a_missing_credential_refuses_rather_than_guessing() {
    let (_context, counter, requests) = counter(None, Vec::new());
    let user = message(Role::User, "count me");
    let request = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&user);
    let failure = counter
        .count(&request, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), TokenCountFailureKind::Unsupported);
    assert!(
        requests.lock().unwrap().is_empty(),
        "no request may be sent without a credential"
    );
}

#[tokio::test]
async fn structured_messages_supported_by_the_endpoint_are_counted_without_flattening() {
    // Current Anthropic primary docs explicitly support images, PDFs, tools,
    // assistant tool calls, and tool results on count_tokens. Their structured
    // blocks must reach the endpoint rather than being flattened or refused.
    let (_context, structured_counter, bodies) = structured_counter(1);
    let image = heycode_llm::ChatImage::new(
        heycode_core::AttachmentMediaType::new("image/png").unwrap(),
        vec![1],
    )
    .unwrap();
    let document = heycode_llm::ChatDocument::new(
        heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
        "reference.pdf",
        b"%PDF-1.7\nfixture".to_vec(),
    )
    .unwrap();
    let with_media =
        heycode_llm::ChatMessage::user_with_media("caption", vec![image], vec![document]);
    let with_tool_call = heycode_llm::ChatMessage::assistant_with_tool_calls(
        "",
        vec![heycode_llm::ChatToolCall {
            id: "call_1".to_owned(),
            name: "read".to_owned(),
            arguments: "{}".to_owned(),
        }],
    );
    let tool_result = heycode_llm::ChatMessage::tool_result("call_1", "failed", true);
    let structured = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&with_media)
        .with_message(&with_tool_call)
        .with_message(&tool_result);
    assert_eq!(
        structured_counter
            .count(&structured, &CancellationToken::new())
            .await
            .unwrap(),
        42
    );
    {
        let bodies = bodies.lock().unwrap();
        let messages = bodies[0]["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][0]["type"], "image");
        assert_eq!(messages[0]["content"][1]["type"], "document");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["is_error"], true);
    }

    // A system-only request has no countable transcript; zero would be a lie.
    let (_context, counter, requests) = counter(Some(TEST_SECRET), Vec::new());
    let system = message(Role::System, "be terse");
    let system_only = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&system);
    assert_eq!(
        counter
            .count(&system_only, &CancellationToken::new())
            .await
            .unwrap_err()
            .kind(),
        TokenCountFailureKind::Unsupported
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn context_editing_count_exposes_original_and_effective_pressure() {
    let (context, credentials) = credentials(Some(TEST_SECRET));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let response = HttpResponse {
        status: 200,
        content_type: Some("application/json".to_owned()),
        headers: BTreeMap::new(),
        body: serde_json::json!({
            "input_tokens":25_000,
            "context_management":{"original_input_tokens":70_000}
        })
        .to_string()
        .into_bytes(),
    };
    let policy = AnthropicContextEditingPolicy::new(
        Some(AnthropicThinkingClear::new(AnthropicThinkingKeep::Turns(2)).unwrap()),
        None,
    )
    .unwrap();
    let counter = AnthropicTokenCounter::new(
        HttpService::new(Arc::new(BodyRecordingTransport {
            bodies: bodies.clone(),
            responses: Mutex::new(vec![response]),
        })),
        (*credentials).clone(),
        AnthropicTokenCounterConfig::official(query()).with_context_editing(policy),
    )
    .unwrap();
    let user = heycode_llm::ChatMessage::user("continue");
    let request = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&user);
    let report = counter
        .count_report(&request, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.effective_input_tokens(), 25_000);
    assert_eq!(report.original_input_tokens(), Some(70_000));
    assert_eq!(report.cleared_input_tokens(), Some(45_000));
    assert_eq!(
        bodies.lock().unwrap()[0]["context_management"]["edits"][0]["type"],
        "clear_thinking_20251015"
    );
    drop(context);
}

#[tokio::test]
async fn a_rejected_or_malformed_reply_fails_without_exposing_the_body() {
    for body in [
        serde_json::json!({"error": {"message": "private-body-canary"}}),
        serde_json::json!({"unexpected": 1}),
    ] {
        let status = if body.get("error").is_some() {
            400
        } else {
            200
        };
        let (_context, counter, _requests) =
            counter(Some(TEST_SECRET), vec![response(status, body)]);
        let user = message(Role::User, "count me");
        let request = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
            .unwrap()
            .with_message(&user);
        let failure = counter
            .count(&request, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.kind(), TokenCountFailureKind::Failed);
        let rendered = format!("{failure:?}");
        assert!(
            !rendered.contains("private-body-canary"),
            "a provider body must never reach a diagnostic: {rendered}"
        );
    }
}

#[tokio::test]
async fn cancellation_settles_before_any_request_is_sent() {
    let (_context, counter, requests) = counter(Some(TEST_SECRET), Vec::new());
    let user = message(Role::User, "count me");
    let request = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&user);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        counter
            .count(&request, &cancellation)
            .await
            .unwrap_err()
            .kind(),
        TokenCountFailureKind::Cancelled
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_registry_prefers_the_measured_counter_over_the_local_estimate() {
    use heycode_llm::{HeuristicTokenEstimator, TokenCounterRegistry};

    let context = heycode_core::Context::new();
    let registry = TokenCounterRegistry::default();
    // Registered estimate-first, so a win here cannot come from ordering.
    registry
        .register(&context, Arc::new(HeuristicTokenEstimator::new()))
        .unwrap();
    let (_credential_context, credentials) = credentials(Some(TEST_SECRET));
    let (service, _requests) = http(vec![response(200, serde_json::json!({"input_tokens": 7}))]);
    registry
        .register(
            &context,
            Arc::new(
                AnthropicTokenCounter::new(
                    service,
                    (*credentials).clone(),
                    AnthropicTokenCounterConfig::official(query()),
                )
                .unwrap(),
            ),
        )
        .unwrap();

    let user = message(Role::User, "count me");
    let request = TokenCountRequest::new("anthropic", ANTHROPIC_CLAUDE_OPUS_5)
        .unwrap()
        .with_message(&user);
    let outcome = registry
        .count(&request, &CancellationToken::new())
        .await
        .unwrap();
    let estimate = outcome
        .count()
        .estimated()
        .expect("a provider-tokenizer estimate must outrank a local estimate");
    assert_eq!(estimate.tokens(), 7);
    assert_eq!(
        estimate.method(),
        heycode_llm::EstimationMethod::ProviderTokenizer
    );
    assert_eq!(estimate.counter().as_str(), ANTHROPIC_TOKEN_COUNTER_ID);
}

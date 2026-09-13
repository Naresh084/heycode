//! PAWS05 Amazon Bedrock Mantle Responses and Messages profiles.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_authorization_aws::{
    AWS_BEDROCK_API_KEY_REFERENCE, AwsProfileResolution, AwsRegion, AwsRegionResolution,
    ProcessAwsHost,
};
use heycode_credentials::CredentialSecret;
use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream, TransportError,
};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, Provider, ProviderProtocol, RequestDraft, ResolveError,
};
use heycode_provider_aws::{
    MANTLE_PROVIDER, MantleMessagesProvider, MantleProtocol, MantleResponsesProvider,
    mantle_messages_base_url, mantle_messages_profile, mantle_responses_base_url,
    mantle_responses_profile,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    ROTATED_TEST_SECRET, TEST_REGION, TEST_SECRET, process_credentials, query, query_for,
    rotating_credentials,
};

const MODEL: &str = "anthropic.claude-fixture-v1";
const RESPONSES_MODEL: &str = "openai.gpt-5.6-sol";

#[derive(Clone)]
struct CapturedRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Default)]
struct RecordingTransport {
    scripts: Mutex<Vec<Vec<SseEvent>>>,
    requests: Mutex<Vec<CapturedRequest>>,
}

struct CancellationTransport;

impl HttpTransport for CancellationTransport {
    fn sse(&self, _request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::once(async move {
            cancellation.cancelled().await;
            Err(TransportError::Cancelled)
        }))
    }
}

impl RecordingTransport {
    fn with_script(script: Vec<SseEvent>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(vec![script]),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for RecordingTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.requests.lock().unwrap().push(CapturedRequest {
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().unwrap_or_default().to_vec(),
        });
        let mut scripts = self.scripts.lock().unwrap();
        let script = if scripts.is_empty() {
            Vec::new()
        } else {
            scripts.remove(0)
        };
        Box::pin(futures::stream::iter(script.into_iter().map(Ok)))
    }
}

fn event(name: &str, data: serde_json::Value) -> SseEvent {
    SseEvent {
        event: name.to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    }
}

fn responses_script() -> Vec<SseEvent> {
    vec![event(
        "response.completed",
        serde_json::json!({
            "type":"response.completed",
            "sequence_number":0,
            "response":{
                "id":"resp_fixture",
                "status":"completed",
                "output":[],
                "usage":{"input_tokens":3,"output_tokens":2}
            }
        }),
    )]
}

fn messages_script() -> Vec<SseEvent> {
    vec![
        event(
            "message_start",
            serde_json::json!({
                "type":"message_start",
                "message":{
                    "id":"msg_fixture",
                    "type":"message",
                    "role":"assistant",
                    "model":MODEL,
                    "content":[],
                    "stop_reason":serde_json::Value::Null,
                    "stop_sequence":serde_json::Value::Null,
                    "usage":{"input_tokens":3,"output_tokens":0}
                }
            }),
        ),
        event(
            "content_block_start",
            serde_json::json!({
                "type":"content_block_start",
                "index":0,
                "content_block":{"type":"text","text":""}
            }),
        ),
        event(
            "content_block_delta",
            serde_json::json!({
                "type":"content_block_delta",
                "index":0,
                "delta":{"type":"text_delta","text":"hi"}
            }),
        ),
        event(
            "content_block_stop",
            serde_json::json!({"type":"content_block_stop","index":0}),
        ),
        event(
            "message_delta",
            serde_json::json!({
                "type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":serde_json::Value::Null},
                "usage":{"output_tokens":1}
            }),
        ),
        event("message_stop", serde_json::json!({"type":"message_stop"})),
    ]
}

fn region() -> AwsRegion {
    AwsRegion::new(TEST_REGION).unwrap()
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: MODEL.to_owned(),
        display_name: MODEL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            structured_output: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: MANTLE_PROVIDER.to_owned(),
        model: MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: Some("Be concise.".to_owned()),
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
        purpose: CallPurpose::Conversation,
    }
}

fn model_for(id: &str) -> ModelDescriptor {
    let mut descriptor = model();
    descriptor.id = id.to_owned();
    descriptor.display_name = id.to_owned();
    descriptor
}

fn draft_for(id: &str) -> RequestDraft {
    let mut request = draft();
    request.model = id.to_owned();
    request
}

fn has_header(request: &CapturedRequest, name: &str, value: &str) -> bool {
    request
        .headers
        .iter()
        .any(|(candidate, candidate_value)| candidate == name && candidate_value == value)
}

#[test]
fn both_profiles_share_catalog_identity_but_select_different_wire_protocols() {
    let responses = mantle_responses_profile(RESPONSES_MODEL);
    let messages = mantle_messages_profile(MODEL);
    for profile in [&responses, &messages] {
        assert_eq!(profile.provider.registry_name, MANTLE_PROVIDER);
        assert_eq!(
            profile.provider.credential_reference.as_deref(),
            Some(AWS_BEDROCK_API_KEY_REFERENCE)
        );
    }
    assert_eq!(responses.provider.default_model, RESPONSES_MODEL);
    assert_eq!(messages.provider.default_model, MODEL);
    assert_eq!(responses.protocol, MantleProtocol::Responses);
    assert_eq!(messages.protocol, MantleProtocol::Messages);
    assert!(
        responses
            .provider
            .descriptor
            .protocols
            .contains(&ProviderProtocol::OpenAiResponses)
    );
    assert!(
        messages
            .provider
            .descriptor
            .protocols
            .contains(&ProviderProtocol::AnthropicMessages)
    );
    assert_eq!(responses.capabilities, responses.protocol.capabilities());
    assert_eq!(messages.capabilities, messages.protocol.capabilities());
}

#[test]
fn mantle_profiles_report_the_exact_custom_operation_credential_reference() {
    let custom = query_for("mantle-production-key");
    let (context, service, _) = rotating_credentials(Some(TEST_SECRET));
    let responses = MantleResponsesProvider::from_credentials(
        HttpService::new(RecordingTransport::with_script(responses_script())),
        service.as_ref(),
        &custom,
        &region(),
        RESPONSES_MODEL,
    )
    .unwrap();
    let messages = MantleMessagesProvider::from_credentials(
        HttpService::new(RecordingTransport::with_script(messages_script())),
        service.as_ref(),
        &custom,
        &region(),
        MODEL,
        None,
    )
    .unwrap();
    assert_eq!(
        responses.credential_reference(),
        Some("mantle-production-key")
    );
    assert_eq!(
        messages.credential_reference(),
        Some("mantle-production-key")
    );
    drop(context);
}

#[test]
fn capability_differences_stay_on_the_selected_mantle_protocol() {
    let responses = MantleProtocol::Responses.capabilities();
    assert_eq!(responses.background, CapabilitySupport::Supported);
    assert_eq!(responses.server_side_tools, CapabilitySupport::Supported);
    assert_eq!(responses.projects, CapabilitySupport::Supported);
    assert_eq!(responses.workspaces, CapabilitySupport::Unsupported);

    let messages = MantleProtocol::Messages.capabilities();
    assert_eq!(messages.background, CapabilitySupport::Unsupported);
    assert_eq!(messages.server_side_tools, CapabilitySupport::Unknown);
    assert_eq!(messages.projects, CapabilitySupport::Unsupported);
    assert_eq!(messages.workspaces, CapabilitySupport::Supported);
    assert_eq!(messages.structured_output, CapabilitySupport::Unsupported);
    assert_eq!(
        MantleProtocol::Responses.model_family_support("anthropic.claude-sonnet-5"),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        MantleProtocol::Responses.model_family_support(RESPONSES_MODEL),
        CapabilitySupport::Unknown,
        "provider family is not exact model compatibility evidence"
    );
    assert_eq!(
        MantleProtocol::Messages.model_family_support(RESPONSES_MODEL),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        MantleProtocol::Messages.model_family_support(MODEL),
        CapabilitySupport::Unknown,
        "provider family is not exact model compatibility evidence"
    );
}

#[test]
fn each_profile_uses_the_documented_mantle_base_path() {
    assert_eq!(
        mantle_responses_base_url(&region()),
        "https://bedrock-mantle.us-east-1.api.aws/v1"
    );
    assert_eq!(
        mantle_messages_base_url(&region()),
        "https://bedrock-mantle.us-east-1.api.aws/anthropic/v1"
    );
}

#[tokio::test]
async fn responses_uses_bearer_auth_and_the_openai_path() {
    let transport = RecordingTransport::with_script(responses_script());
    let provider = MantleResponsesProvider::new(
        HttpService::new(transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        RESPONSES_MODEL,
    )
    .unwrap();
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter
        .resolve(draft_for(RESPONSES_MODEL), &model_for(RESPONSES_MODEL))
        .unwrap();
    let events: Vec<_> = adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://bedrock-mantle.us-east-1.api.aws/v1/responses"
    );
    assert!(has_header(
        &requests[0],
        "authorization",
        &format!("Bearer {TEST_SECRET}")
    ));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["store"], serde_json::json!(false));
    assert_eq!(body["model"], serde_json::json!(RESPONSES_MODEL));
}

#[tokio::test]
async fn messages_uses_x_api_key_version_header_and_the_anthropic_path() {
    let transport = RecordingTransport::with_script(messages_script());
    let provider = MantleMessagesProvider::new(
        HttpService::new(transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        MODEL,
        None,
    )
    .unwrap();
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter.resolve(draft(), &model()).unwrap();
    let events: Vec<_> = adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(_))))
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://bedrock-mantle.us-east-1.api.aws/anthropic/v1/messages"
    );
    assert!(has_header(&requests[0], "x-api-key", TEST_SECRET));
    assert!(has_header(&requests[0], "anthropic-version", "2023-06-01"));
    assert!(
        !requests[0]
            .headers
            .iter()
            .any(|(name, _)| name == "authorization")
    );
}

#[tokio::test]
async fn mantle_messages_refuses_structured_output_before_transport() {
    let transport = RecordingTransport::with_script(messages_script());
    let provider = MantleMessagesProvider::new(
        HttpService::new(transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        MODEL,
        None,
    )
    .unwrap();
    let mut request = draft();
    request.structured_output = Some(serde_json::json!({
        "type":"object",
        "properties":{"answer":{"type":"string"}},
        "required":["answer"],
        "additionalProperties":false
    }));
    let error = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, &model())
        .expect_err("Mantle Messages rejects output_config.format");
    assert!(
        matches!(error, ResolveError::InvalidRequest { field, .. } if field == "structured_output")
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn mantle_responses_refuses_an_anthropic_messages_model_before_transport() {
    let transport = RecordingTransport::with_script(responses_script());
    let provider = MantleResponsesProvider::new(
        HttpService::new(transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        RESPONSES_MODEL,
    )
    .unwrap();
    let error = provider
        .inference_adapter()
        .unwrap()
        .resolve(draft(), &model())
        .expect_err("the current AWS matrix marks Anthropic models Responses-unsupported");
    assert!(matches!(error, ResolveError::InvalidRequest { field, .. } if field == "model"));
    assert!(transport.requests().is_empty());
}

#[test]
fn mantle_messages_refuses_an_openai_responses_model_before_transport() {
    let transport = RecordingTransport::with_script(messages_script());
    let provider = MantleMessagesProvider::new(
        HttpService::new(transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        MODEL,
        None,
    )
    .unwrap();
    let incompatible = "openai.gpt-5.6-sol";
    let mut request = draft();
    request.model = incompatible.to_owned();
    let mut descriptor = model();
    descriptor.id = incompatible.to_owned();
    descriptor.display_name = incompatible.to_owned();
    let error = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, &descriptor)
        .expect_err("the current AWS matrix marks OpenAI models Messages-unsupported");
    assert!(matches!(error, ResolveError::InvalidRequest { field, .. } if field == "model"));
    assert!(transport.requests().is_empty());
}

#[test]
fn incompatible_mantle_default_models_fail_during_provider_construction() {
    let responses_transport = RecordingTransport::with_script(responses_script());
    let responses = MantleResponsesProvider::new(
        HttpService::new(responses_transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        MODEL,
    );
    assert!(responses.is_err());
    assert!(responses_transport.requests().is_empty());

    let messages_transport = RecordingTransport::with_script(messages_script());
    let messages = MantleMessagesProvider::new(
        HttpService::new(messages_transport.clone()),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        RESPONSES_MODEL,
        None,
    );
    assert!(messages.is_err());
    assert!(messages_transport.requests().is_empty());
}

#[tokio::test]
async fn both_mantle_profiles_resolve_the_exact_key_once_per_operation() {
    let responses_transport = RecordingTransport::with_script(responses_script());
    responses_transport
        .scripts
        .lock()
        .unwrap()
        .push(responses_script());
    let (responses_context, responses_service, responses_secret) =
        rotating_credentials(Some(TEST_SECRET));
    let responses = MantleResponsesProvider::from_credentials(
        HttpService::new(responses_transport.clone()),
        responses_service.as_ref(),
        &query(),
        &region(),
        RESPONSES_MODEL,
    )
    .unwrap();
    let adapter = responses.inference_adapter().unwrap();
    assert!(matches!(
        adapter.authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(_)
    ));
    let first = adapter
        .resolve(draft_for(RESPONSES_MODEL), &model_for(RESPONSES_MODEL))
        .unwrap();
    let _: Vec<_> = adapter
        .stream_cancellable(first, CancellationToken::new())
        .collect()
        .await;
    responses_secret.set(Some(ROTATED_TEST_SECRET));
    let second = adapter
        .resolve(draft_for(RESPONSES_MODEL), &model_for(RESPONSES_MODEL))
        .unwrap();
    let _: Vec<_> = adapter
        .stream_cancellable(second, CancellationToken::new())
        .collect()
        .await;
    let requests = responses_transport.requests();
    assert_eq!(requests.len(), 2);
    assert!(has_header(
        &requests[0],
        "authorization",
        &format!("Bearer {TEST_SECRET}")
    ));
    assert!(has_header(
        &requests[1],
        "authorization",
        &format!("Bearer {ROTATED_TEST_SECRET}")
    ));
    drop(responses_context);

    let messages_transport = RecordingTransport::with_script(messages_script());
    messages_transport
        .scripts
        .lock()
        .unwrap()
        .push(messages_script());
    let (messages_context, messages_service, messages_secret) =
        rotating_credentials(Some(TEST_SECRET));
    let messages = MantleMessagesProvider::from_credentials(
        HttpService::new(messages_transport.clone()),
        messages_service.as_ref(),
        &query(),
        &region(),
        MODEL,
        None,
    )
    .unwrap();
    let adapter = messages.inference_adapter().unwrap();
    assert!(matches!(
        adapter.authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(_)
    ));
    let first = adapter.resolve(draft(), &model()).unwrap();
    let _: Vec<_> = adapter
        .stream_cancellable(first, CancellationToken::new())
        .collect()
        .await;
    messages_secret.set(Some(ROTATED_TEST_SECRET));
    let second = adapter.resolve(draft(), &model()).unwrap();
    let _: Vec<_> = adapter
        .stream_cancellable(second, CancellationToken::new())
        .collect()
        .await;
    let requests = messages_transport.requests();
    assert_eq!(requests.len(), 2);
    assert!(has_header(&requests[0], "x-api-key", TEST_SECRET));
    assert!(has_header(&requests[1], "x-api-key", ROTATED_TEST_SECRET));
    drop(messages_context);
}

#[tokio::test]
async fn mantle_profile_wrapper_threads_the_callers_cancellation_into_transport() {
    let provider = MantleResponsesProvider::new(
        HttpService::new(Arc::new(CancellationTransport)),
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        RESPONSES_MODEL,
    )
    .unwrap();
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter
        .resolve(draft_for(RESPONSES_MODEL), &model_for(RESPONSES_MODEL))
        .unwrap();
    let cancellation = CancellationToken::new();
    let mut events = adapter.stream_cancellable(call, cancellation.clone());
    cancellation.cancel();
    let event = tokio::time::timeout(std::time::Duration::from_millis(100), events.next())
        .await
        .expect("caller cancellation must settle the provider stream")
        .expect("cancellation must yield one terminal failure");
    assert!(event.is_err(), "{event:?}");
}

async fn assert_live_mantle_turn(adapter: &dyn InferenceAdapter, model_id: &str) {
    let mut descriptor = model();
    descriptor.id = model_id.to_owned();
    descriptor.display_name = model_id.to_owned();
    descriptor.capabilities = ModelCapabilities::unknown();
    let mut request = draft();
    request.model = model_id.to_owned();
    request.max_output_tokens = Some(128);
    let call = adapter
        .resolve(request, &descriptor)
        .expect("the configured Mantle text turn must resolve");
    let events: Vec<_> = adapter
        .stream_cancellable(call, CancellationToken::new())
        .collect()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Finish(_)))),
        "a live Mantle turn must settle"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::Usage(_)))),
        "a live Mantle turn must publish usage"
    );
}

/// Hosted Mantle canary. It runs only with an explicit API family, model,
/// region and process-scoped key; no credential value enters diagnostics.
#[tokio::test]
async fn live_mantle_profile_smoke() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    if std::env::var_os("AWS_BEARER_TOKEN_BEDROCK")
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return;
    }
    let host = ProcessAwsHost;
    let profile = AwsProfileResolution::resolve(&host);
    let resolution = AwsRegionResolution::resolve(&host, &profile);
    let Some(region) = resolution.region().cloned() else {
        return;
    };
    let Some(model_id) = std::env::var("HEYCODE_E2E_BEDROCK_MANTLE_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    let Some(protocol) = std::env::var("HEYCODE_E2E_BEDROCK_MANTLE_PROTOCOL").ok() else {
        return;
    };
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let http = HttpService::new(Arc::new(transport));
    let (context, credentials) = process_credentials();
    match protocol.as_str() {
        "responses" => {
            let provider = MantleResponsesProvider::from_credentials(
                http,
                credentials.as_ref(),
                &query(),
                &region,
                model_id.clone(),
            )
            .unwrap();
            assert_live_mantle_turn(provider.inference_adapter().unwrap(), &model_id).await;
        }
        "messages" => {
            let provider = MantleMessagesProvider::from_credentials(
                http,
                credentials.as_ref(),
                &query(),
                &region,
                model_id.clone(),
                None,
            )
            .unwrap();
            assert_live_mantle_turn(provider.inference_adapter().unwrap(), &model_id).await;
        }
        _ => panic!("HEYCODE_E2E_BEDROCK_MANTLE_PROTOCOL must be `responses` or `messages`"),
    }
    drop(context);
}

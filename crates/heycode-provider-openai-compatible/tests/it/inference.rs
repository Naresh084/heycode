use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_credentials::{CredentialResolutionError, CredentialSecret};
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    AuthenticationBinding, CallPurpose, ChatMessage, CredentialHandle, CredentialResolver,
    InferenceInput, InputModality, Provider, RequestDraft, RouteCredential,
};
use heycode_provider_openai_compatible::{
    CUSTOM_OPENAI_PROVIDER, CustomOpenAiEndpoint, CustomOpenAiModel, CustomOpenAiProvider,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Captured {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct CaptureTransport(Mutex<Vec<Captured>>);

impl HttpTransport for CaptureTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.0.lock().unwrap().push(Captured {
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().unwrap_or_default().to_vec(),
        });
        Box::pin(futures::stream::iter([
            Ok(SseEvent {
                event: "message".to_owned(),
                data: serde_json::json!({
                    "id":"chat_fixture",
                    "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":1,"completion_tokens":1}
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
}

struct RotatingResolver {
    route: CredentialHandle,
    secret: Mutex<String>,
}

impl RotatingResolver {
    fn rotate(&self, value: &str) {
        *self.secret.lock().unwrap() = value.to_owned();
    }
}

impl CredentialResolver for RotatingResolver {
    fn route(&self) -> &CredentialHandle {
        &self.route
    }

    fn resolve(
        &self,
        _route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        Ok(CredentialSecret::new(self.secret.lock().unwrap().clone()))
    }
}

fn draft(model: &str) -> RequestDraft {
    RequestDraft {
        provider: CUSTOM_OPENAI_PROVIDER.to_owned(),
        model: model.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: vec![heycode_llm::ToolSpec {
            name: "read".to_owned(),
            description: "Read a workspace file".to_owned(),
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        }],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Evaluation,
    }
}

fn provider(
    transport: Arc<CaptureTransport>,
    credential: Option<RouteCredential>,
) -> CustomOpenAiProvider {
    CustomOpenAiProvider::new(
        heycode_http::HttpService::new(transport),
        CustomOpenAiEndpoint::new("http://localhost:8000/v1").unwrap(),
        CustomOpenAiModel::new("local/model").unwrap(),
        credential,
    )
    .unwrap()
}

async fn dispatch(provider: &CustomOpenAiProvider) {
    assert_eq!(
        provider.describe_model("local/model").capabilities.tools,
        heycode_llm::CapabilitySupport::Unknown
    );
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter
        .resolve(
            draft("local/model"),
            &provider.describe_model("local/model"),
        )
        .unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:?}");
}

#[tokio::test]
async fn unauthenticated_route_sends_exact_chat_request_without_authorization() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let provider = provider(transport.clone(), None);
    assert_eq!(
        provider
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        AuthenticationBinding::None
    );
    assert_eq!(provider.credential_reference(), None);
    dispatch(&provider).await;

    let captured = transport.0.lock().unwrap()[0].clone();
    assert_eq!(captured.url, "http://localhost:8000/v1/chat/completions");
    assert!(
        captured
            .headers
            .iter()
            .all(|(name, _)| name != "authorization")
    );
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["model"], "local/model");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn optional_key_rotation_reaches_the_next_operation_as_bearer_auth() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let resolver = Arc::new(RotatingResolver {
        route: CredentialHandle::new("HEYCODE_ENDPOINT_TEST").unwrap(),
        secret: Mutex::new("fixture-first".to_owned()),
    });
    let provider = provider(
        transport.clone(),
        Some(RouteCredential::per_operation(
            resolver.route.clone(),
            resolver.clone(),
        )),
    );
    assert_eq!(
        provider.credential_reference(),
        Some("HEYCODE_ENDPOINT_TEST")
    );

    dispatch(&provider).await;
    resolver.rotate("fixture-rotated");
    dispatch(&provider).await;

    let requests = transport.0.lock().unwrap();
    for (request, expected) in requests
        .iter()
        .zip(["Bearer fixture-first", "Bearer fixture-rotated"])
    {
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == expected)
        );
    }
}

#[test]
fn selected_model_is_the_only_model_admitted_before_io() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let provider = provider(transport.clone(), None);
    assert!(
        provider
            .inference_adapter()
            .unwrap()
            .resolve(draft("other"), &provider.describe_model("other"))
            .is_err()
    );
    assert!(transport.0.lock().unwrap().is_empty());
}

#[test]
fn endpoint_and_model_boundaries_reject_ambiguous_or_secret_bearing_values() {
    for value in [
        "",
        " http://localhost/v1",
        "ftp://localhost/v1",
        "http://name:key@localhost/v1",
        "http://localhost/v1?token=secret",
        "http://localhost/v1#part",
    ] {
        assert!(CustomOpenAiEndpoint::new(value).is_err(), "{value}");
    }
    for value in ["", " model", "model\nother"] {
        assert!(CustomOpenAiModel::new(value).is_err(), "{value:?}");
    }
}

#[test]
fn unknown_tool_attempts_do_not_admit_unsupported_tools_or_other_unknown_features() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let provider = provider(transport.clone(), None);
    let adapter = provider.inference_adapter().unwrap();
    let mut model = provider.describe_model("local/model");
    model.capabilities.tools = heycode_llm::CapabilitySupport::Unsupported;
    assert!(matches!(
        adapter.resolve(draft("local/model"), &model),
        Err(heycode_llm::ResolveError::Unsupported {
            capability: heycode_llm::RequestedCapability::Tools,
            ..
        })
    ));
    model.capabilities.tools = heycode_llm::CapabilitySupport::Unknown;
    let mut request = draft("local/model");
    request.reasoning_effort = Some(heycode_llm::ReasoningEffortId::new("high").unwrap());
    assert!(matches!(
        adapter.resolve(request, &model),
        Err(heycode_llm::ResolveError::Unproven {
            capability: heycode_llm::RequestedCapability::Reasoning,
            ..
        })
    ));
    let mut request = draft("local/model");
    request.tools[0].parameters = serde_json::Value::Null;
    assert!(adapter.resolve(request, &model).is_err());
    let strict = heycode_llm::OpenAiChatCompletionsAdapter::new(
        heycode_llm::OpenAiChatCompletionsConfig::with_key(
            provider.descriptor(),
            "https://strict.example/v1",
            "fixture-key",
        ),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    assert!(matches!(
        heycode_llm::InferenceAdapter::resolve(&strict, draft("local/model"), &model),
        Err(heycode_llm::ResolveError::Unproven {
            capability: heycode_llm::RequestedCapability::Tools,
            ..
        })
    ));
    assert!(transport.0.lock().unwrap().is_empty());
}

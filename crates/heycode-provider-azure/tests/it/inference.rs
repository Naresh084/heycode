use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_credentials::{CredentialResolutionError, CredentialSecret};
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, ChatMessage, CredentialHandle, CredentialResolver, InferenceInput, InputModality,
    Provider, RequestDraft, RouteCredential,
};
use heycode_provider_azure::{AzureDeploymentName, AzureOpenAiProvider, AzureResourceName};
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
        Box::pin(futures::stream::iter([Ok(SseEvent {
            event: "response.completed".to_owned(),
            data: serde_json::json!({
                "type":"response.completed",
                "sequence_number":0,
                "response":{
                    "id":"resp_fixture",
                    "status":"completed",
                    "output":[],
                    "usage":{"input_tokens":1,"output_tokens":1}
                }
            })
            .to_string(),
            id: None,
            retry_ms: None,
        })]))
    }
}

struct RotatingResolver {
    route: CredentialHandle,
    secret: Mutex<String>,
}

impl RotatingResolver {
    fn rotate(&self, secret: &str) {
        *self.secret.lock().unwrap() = secret.to_owned();
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
        provider: "azure-openai".to_owned(),
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

async fn dispatch(provider: &AzureOpenAiProvider) {
    assert_eq!(
        provider.describe_model("prod-gpt").capabilities.tools,
        heycode_llm::CapabilitySupport::Unknown
    );
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter
        .resolve(draft("prod-gpt"), &provider.describe_model("prod-gpt"))
        .unwrap();
    let output = adapter.stream(call).collect::<Vec<_>>().await;
    assert!(output.iter().all(Result::is_ok), "{output:?}");
}

#[tokio::test]
async fn azure_v1_uses_the_exact_resource_deployment_and_api_key_header() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let provider = AzureOpenAiProvider::new(
        heycode_http::HttpService::new(transport.clone()),
        AzureResourceName::new("team-agent").unwrap(),
        AzureDeploymentName::new("prod-gpt").unwrap(),
        "AZURE_OPENAI_API_KEY",
        RouteCredential::fixed("fixture-secret"),
    )
    .unwrap();
    dispatch(&provider).await;

    let captured = transport.0.lock().unwrap()[0].clone();
    assert_eq!(
        captured.url,
        "https://team-agent.openai.azure.com/openai/v1/responses"
    );
    assert!(
        captured
            .headers
            .iter()
            .any(|(name, value)| name == "api-key" && value == "fixture-secret"),
        "{:?}",
        captured.headers
    );
    assert!(
        !captured
            .headers
            .iter()
            .any(|(name, _)| name == "authorization")
    );
    let body: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["model"], "prod-gpt");
    assert_eq!(body["tools"][0]["name"], "read");
}

#[tokio::test]
async fn api_key_rotation_reaches_the_next_operation_without_rebuilding_the_provider() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let resolver = Arc::new(RotatingResolver {
        route: CredentialHandle::new("AZURE_OPENAI_API_KEY").unwrap(),
        secret: Mutex::new("fixture-first".to_owned()),
    });
    let provider = AzureOpenAiProvider::new(
        heycode_http::HttpService::new(transport.clone()),
        AzureResourceName::new("team-agent").unwrap(),
        AzureDeploymentName::new("prod-gpt").unwrap(),
        "AZURE_OPENAI_API_KEY",
        RouteCredential::per_operation(resolver.route.clone(), resolver.clone()),
    )
    .unwrap();

    dispatch(&provider).await;
    resolver.rotate("fixture-rotated");
    dispatch(&provider).await;

    let requests = transport.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name == "api-key" && value == "fixture-first")
    );
    assert!(
        requests[1]
            .headers
            .iter()
            .any(|(name, value)| name == "api-key" && value == "fixture-rotated")
    );
}

#[test]
fn configured_deployment_is_the_only_model_admitted_before_network_io() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let provider = AzureOpenAiProvider::new(
        heycode_http::HttpService::new(transport.clone()),
        AzureResourceName::new("team-agent").unwrap(),
        AzureDeploymentName::new("prod-gpt").unwrap(),
        "AZURE_OPENAI_API_KEY",
        RouteCredential::fixed("fixture-secret"),
    )
    .unwrap();
    let adapter = provider.inference_adapter().unwrap();
    assert!(
        adapter
            .resolve(draft("other"), &provider.describe_model("other"))
            .is_err()
    );
    assert!(transport.0.lock().unwrap().is_empty());
}

#[test]
fn coordinates_reject_host_and_path_injection() {
    for value in ["a", "-team", "team.", "team/other", "team_name"] {
        assert!(AzureResourceName::new(value).is_err(), "{value}");
    }
    for value in ["", " prod", "prod/other", "prod?x=1", "prod#fragment"] {
        assert!(AzureDeploymentName::new(value).is_err(), "{value}");
    }
}

#[test]
fn unknown_tool_attempts_do_not_admit_unsupported_tools_or_other_unknown_features() {
    let transport = Arc::new(CaptureTransport(Mutex::new(Vec::new())));
    let provider = AzureOpenAiProvider::new(
        heycode_http::HttpService::new(transport.clone()),
        AzureResourceName::new("team-agent").unwrap(),
        AzureDeploymentName::new("prod-gpt").unwrap(),
        "AZURE_OPENAI_API_KEY",
        RouteCredential::fixed("fixture-secret"),
    )
    .unwrap();
    let adapter = provider.inference_adapter().unwrap();
    let mut model = provider.describe_model("prod-gpt");
    model.capabilities.tools = heycode_llm::CapabilitySupport::Unsupported;
    assert!(matches!(
        adapter.resolve(draft("prod-gpt"), &model),
        Err(heycode_llm::ResolveError::Unsupported {
            capability: heycode_llm::RequestedCapability::Tools,
            ..
        })
    ));
    model.capabilities.tools = heycode_llm::CapabilitySupport::Unknown;
    let mut request = draft("prod-gpt");
    request.reasoning_effort = Some(heycode_llm::ReasoningEffortId::new("high").unwrap());
    assert!(matches!(
        adapter.resolve(request, &model),
        Err(heycode_llm::ResolveError::Unproven {
            capability: heycode_llm::RequestedCapability::Reasoning,
            ..
        })
    ));
    let mut request = draft("prod-gpt");
    request.tools[0].parameters = serde_json::Value::Null;
    assert!(adapter.resolve(request, &model).is_err());
    let strict = heycode_llm::OpenAiResponsesAdapter::new(
        heycode_llm::OpenAiResponsesConfig::with_key(
            provider.descriptor(),
            "https://strict.example/v1",
            "fixture-key",
        ),
        heycode_http::HttpService::new(transport.clone()),
    )
    .unwrap();
    assert!(matches!(
        heycode_llm::InferenceAdapter::resolve(&strict, draft("prod-gpt"), &model),
        Err(heycode_llm::ResolveError::Unproven {
            capability: heycode_llm::RequestedCapability::Tools,
            ..
        })
    ));
    assert!(transport.0.lock().unwrap().is_empty());
}

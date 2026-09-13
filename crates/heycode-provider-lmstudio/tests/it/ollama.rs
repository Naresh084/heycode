//! PLM05: Ollama is a distinct sibling identity with explicit smoke prerequisites.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt as _;
use heycode_core::{Context, CoreError, Plugin, ProviderProtocol, compose};
use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, SERVICE_HTTP, SseEvent, SseEventStream,
};
use heycode_llm::ModelCatalog as _;
use heycode_llm::{
    CallPurpose, CatalogRefreshMode, CatalogRegistry, ChatMessage, InferenceAdapter,
    InferenceEvent, InferenceInput, InputModality, ModelDescriptor, Provider, RequestDraft,
    SERVICE_MODELS,
};
use heycode_provider_lmstudio::{
    LM_STUDIO_PROVIDER, OLLAMA_DEFAULT_BASE_URL, OLLAMA_PROVIDER, OllamaCatalog, OllamaEndpoint,
    OllamaInference, OllamaInspector, OllamaProfile, OllamaReadinessError, SERVICE_OLLAMA_CATALOG,
    SERVICE_OLLAMA_INFERENCE, SERVICE_OLLAMA_INSPECTOR, SERVICE_OLLAMA_PROFILE,
    ollama_catalog_plugin, ollama_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{Outcome, ScriptedTransport, service};

const VERSION: &str = r#"{"version":"0.12.6"}"#;

#[tokio::test]
async fn ollama_explicit_key_covers_native_discovery_without_becoming_a_default() {
    let transport = product_transport();
    let catalog = OllamaCatalog::new(service(&transport), OllamaEndpoint::local());
    let key = heycode_credentials::CredentialSecret::new("fixture-ollama-key");
    let models = catalog
        .fetch_endpoint_with_credential(
            OLLAMA_DEFAULT_BASE_URL,
            Some(&key),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(models[0].id, "gemma4");
    let count = transport.requests().len();
    assert!(count >= 5);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.authorization.as_deref() == Some("Bearer fixture-ollama-key"))
    );
    catalog
        .fetch_endpoint(OLLAMA_DEFAULT_BASE_URL, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        transport.requests()[count..]
            .iter()
            .all(|request| request.authorization.is_none())
    );
}

#[tokio::test]
async fn ollama_rejected_key_is_authorization_failure_not_network_failure() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Response(
        401,
        Some("application/json"),
        "{}",
    )));
    let catalog = OllamaCatalog::new(service(&transport), OllamaEndpoint::local());
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), heycode_llm::CatalogFailureKind::Unauthorized);
}
const TAGS: &str = r#"{
  "models":[{
    "name":"gemma3","model":"gemma3",
    "modified_at":"2025-10-03T23:34:03.409490317-07:00",
    "size":3338801804,
    "digest":"a2af6cc3eb7fa8be8504abaf9b04e88f17a119ec3f04a3addf55f92841195f5a",
    "details":{"format":"gguf","family":"gemma","families":["gemma"],
      "parameter_size":"4.3B","quantization_level":"Q4_K_M"}
  }]}"#;
const OPENAI_MODELS: &str =
    r#"{"object":"list","data":[{"id":"gemma3","object":"model","owned_by":"library"}]}"#;
const PS: &str = r#"{"models":[]}"#;
const SHOW: &str = r#"{
  "modified_at":"2025-10-03T23:34:03.409490317-07:00",
  "capabilities":["completion"],
  "model_info":{"general.architecture":"gemma","gemma.context_length":131072}
}"#;

const CURRENT_VERSION: &str = r#"{"version":"0.13.3"}"#;
const CURRENT_TAGS: &str = r#"{
  "models":[{
    "name":"gemma4","model":"gemma4",
    "modified_at":"2025-10-03T23:34:03.409490317-07:00",
    "size":9608350245,
    "digest":"c6eb396dbd5992bbe3f5cdb947e8bbc0ee413d7c17e2beaae69f5d569cf982eb",
    "details":{"format":"gguf","family":"gemma4","families":["gemma4"],
      "parameter_size":"8.0B","quantization_level":"Q4_K_M"}
  }]}
"#;
const CURRENT_PS: &str = r#"{
  "models":[{
    "name":"gemma4","model":"gemma4","size":6591830464,
    "digest":"c6eb396dbd5992bbe3f5cdb947e8bbc0ee413d7c17e2beaae69f5d569cf982eb",
    "details":{"format":"gguf","family":"gemma4","families":["gemma4"],
      "parameter_size":"8.0B","quantization_level":"Q4_K_M"},
    "expires_at":"2025-10-17T16:47:07.93355-07:00",
    "size_vram":5333539264,"context_length":4096
  }]}
"#;
const CURRENT_SHOW: &str = r#"{
  "modified_at":"2025-10-03T23:34:03.409490317-07:00",
  "details":{"parent_model":"","format":"gguf","family":"gemma4",
    "families":["gemma4"],"parameter_size":"8.0B","quantization_level":"Q4_K_M"},
  "capabilities":["completion","vision","tools"],
  "model_info":{"general.architecture":"gemma4","gemma4.context_length":131072}
}"#;
const EMBEDDING_SHOW: &str = r#"{
  "modified_at":"2025-10-03T23:34:03.409490317-07:00",
  "capabilities":["embedding","future-provider-capability"],
  "model_info":{"general.architecture":"gemma4","gemma4.context_length":131072}
}"#;
const CURRENT_OPENAI_MODELS: &str = r#"{"object":"list","data":[{"id":"gemma4","object":"model","created":1760000000,"owned_by":"library"}]}"#;

fn transport() -> Arc<ScriptedTransport> {
    Arc::new(
        ScriptedTransport::new(Outcome::Refused)
            .on_url("http://localhost:11434/api/version", Outcome::Json(VERSION))
            .on_url("http://localhost:11434/api/tags", Outcome::Json(TAGS))
            .on_url("http://localhost:11434/api/ps", Outcome::Json(PS))
            .on_url("http://localhost:11434/api/show", Outcome::Json(SHOW))
            .on_url(
                "http://localhost:11434/v1/models",
                Outcome::Json(OPENAI_MODELS),
            ),
    )
}

fn product_transport() -> Arc<ScriptedTransport> {
    product_transport_with_show(CURRENT_SHOW)
}

fn product_transport_with_show(show: &'static str) -> Arc<ScriptedTransport> {
    Arc::new(
        ScriptedTransport::new(Outcome::Refused)
            .on_url(
                "http://localhost:11434/api/version",
                Outcome::Json(CURRENT_VERSION),
            )
            .on_url(
                "http://localhost:11434/api/tags",
                Outcome::Json(CURRENT_TAGS),
            )
            .on_url("http://localhost:11434/api/ps", Outcome::Json(CURRENT_PS))
            .on_url("http://localhost:11434/api/show", Outcome::Json(show))
            .on_url(
                "http://localhost:11434/v1/models",
                Outcome::Json(CURRENT_OPENAI_MODELS),
            ),
    )
}

struct HttpPlugin(Arc<ScriptedTransport>);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), service(&self.0))
    }
}

#[derive(Clone)]
struct CapturedSmokeRequest {
    url: String,
    authorization: Option<String>,
    body: serde_json::Value,
}

struct SmokeTransport {
    events: Mutex<Option<Vec<Result<SseEvent, heycode_http::TransportError>>>>,
    captured: Mutex<Option<CapturedSmokeRequest>>,
}

impl SmokeTransport {
    fn new(events: Vec<Result<SseEvent, heycode_http::TransportError>>) -> Self {
        Self {
            events: Mutex::new(Some(events)),
            captured: Mutex::new(None),
        }
    }
}

impl HttpTransport for SmokeTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        *self.captured.lock().unwrap() = Some(CapturedSmokeRequest {
            url: request.url().to_owned(),
            authorization: request
                .headers()
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("authorization"))
                .map(|header| header.value().to_owned()),
            body: serde_json::from_slice(request.body().unwrap_or_default()).unwrap(),
        });
        Box::pin(futures::stream::iter(
            self.events.lock().unwrap().take().unwrap(),
        ))
    }
}

fn smoke_event(data: serde_json::Value) -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    })
}

#[tokio::test]
async fn ollama_chat_uses_the_explicit_server_key_instead_of_the_placeholder() {
    let transport = Arc::new(SmokeTransport::new(vec![
        smoke_event(serde_json::json!({
            "id":"test", "choices":[{"index":0,"delta":{"content":"ready"},"finish_reason":"stop"}]
        })),
        Ok(SseEvent {
            event: "message".into(),
            data: "[DONE]".into(),
            id: None,
            retry_ms: None,
        }),
    ]));
    let inference = OllamaInference::with_credential(
        HttpService::new(transport.clone()),
        OllamaEndpoint::local(),
        "gemma3",
        heycode_llm::RouteCredential::fixed("fixture-ollama-proxy-key"),
    )
    .unwrap();
    let call = inference
        .resolve(smoke_draft(), &ModelDescriptor::unknown("gemma3"))
        .unwrap();
    let output = InferenceAdapter::stream(&inference, call)
        .collect::<Vec<_>>()
        .await;
    assert!(output.iter().all(Result::is_ok));
    assert_eq!(
        transport
            .captured
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .authorization
            .as_deref(),
        Some("Bearer fixture-ollama-proxy-key")
    );
}

fn smoke_draft() -> RequestDraft {
    RequestDraft {
        provider: OLLAMA_PROVIDER.to_owned(),
        model: "gemma3".to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("say ready"))],
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

#[test]
fn ollama_profile_identity_and_endpoints_never_alias_lm_studio() {
    let endpoint = OllamaEndpoint::local();
    assert_eq!(OLLAMA_DEFAULT_BASE_URL, "http://localhost:11434");
    assert_eq!(endpoint.version_url(), "http://localhost:11434/api/version");
    assert_eq!(endpoint.tags_url(), "http://localhost:11434/api/tags");
    assert_eq!(endpoint.running_url(), "http://localhost:11434/api/ps");
    assert_eq!(endpoint.show_url(), "http://localhost:11434/api/show");
    assert_eq!(endpoint.openai_base_url(), "http://localhost:11434/v1");
    assert_eq!(
        endpoint.openai_models_url(),
        "http://localhost:11434/v1/models"
    );
    assert_ne!(OLLAMA_PROVIDER, LM_STUDIO_PROVIDER);

    let profile = OllamaProfile::new("gemma3").unwrap().provider_profile();
    assert_eq!(profile.registry_name, "ollama");
    assert_eq!(profile.default_model, "gemma3");
    assert_eq!(
        profile.descriptor.protocols,
        vec![ProviderProtocol::OpenAiChatCompletions]
    );
    assert!(profile.credential_reference.is_none());
}

#[tokio::test]
async fn readiness_requires_native_identity_catalog_and_openai_picker_agreement() {
    let transport = transport();
    let inspector = OllamaInspector::new(service(&transport), OllamaEndpoint::local());
    let readiness = inspector
        .inspect("gemma3", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(readiness.version(), "0.12.6");
    assert_eq!(readiness.model(), "gemma3");
    assert!(!readiness.live_smoke_passed());
    let requests = transport.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| request.authorization.is_none())
    );
}

#[tokio::test]
async fn lm_studio_native_shape_cannot_satisfy_ollama_identity() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Refused)
            .on_url(
                "http://localhost:11434/api/version",
                Outcome::Json(r#"{"models":[]}"#),
            )
            .on_url("http://localhost:11434/api/tags", Outcome::Json(TAGS))
            .on_url(
                "http://localhost:11434/v1/models",
                Outcome::Json(OPENAI_MODELS),
            ),
    );
    let inspector = OllamaInspector::new(service(&transport), OllamaEndpoint::local());
    assert_eq!(
        inspector.inspect("gemma3", CancellationToken::new()).await,
        Err(OllamaReadinessError::UnrecognizedVersion)
    );
}

#[tokio::test]
async fn selected_model_must_exist_in_both_native_and_openai_picker_lists() {
    let transport = transport();
    let inspector = OllamaInspector::new(service(&transport), OllamaEndpoint::local());
    assert_eq!(
        inspector.inspect("missing", CancellationToken::new()).await,
        Err(OllamaReadinessError::ModelMissing)
    );
}

#[tokio::test]
async fn native_catalog_keeps_protocol_evidence_separate_from_the_profile() {
    let transport = transport();
    let catalog = OllamaCatalog::new(service(&transport), OllamaEndpoint::local());
    let records = catalog.list_models(CancellationToken::new()).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].name, "gemma3");
    assert_eq!(records[0].details.format.as_deref(), Some("gguf"));
    assert_eq!(catalog.provider_descriptor().id, "ollama");
    assert_eq!(
        catalog.provider_descriptor().protocols,
        vec![ProviderProtocol::Unknown]
    );
}

#[tokio::test]
async fn ollama_catalog_plugin_is_a_distinct_effect_owned_sibling() {
    let plugin = ollama_catalog_plugin(OllamaEndpoint::local());
    assert_eq!(plugin.name(), "catalog-ollama");
    assert_eq!(plugin.descriptor().id, "catalog-ollama");
    assert_eq!(plugin.inject(), &[SERVICE_MODELS, SERVICE_HTTP]);
    let inventory = plugin.inventory();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].name, "ollama");

    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(transport())),
        plugin,
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            OLLAMA_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(view.snapshot.provider.id, "ollama");
    assert_eq!(view.snapshot.models.len(), 1);
    assert_eq!(view.snapshot.models[0].id, "gemma3");

    context.shutdown();
    assert!(
        models
            .refresh(
                OLLAMA_PROVIDER,
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn product_plugin_owns_picker_ready_catalog_profile_inference_and_inspector() {
    let transport = product_transport();
    let profile = OllamaProfile::new("gemma4").unwrap();
    let plugin = ollama_plugin(OllamaEndpoint::local(), profile.clone());
    assert_eq!(plugin.name(), "provider-ollama");
    assert_eq!(plugin.inject(), &[SERVICE_MODELS, SERVICE_HTTP]);
    assert_eq!(
        plugin.provides(),
        &[
            SERVICE_OLLAMA_CATALOG,
            SERVICE_OLLAMA_PROFILE,
            SERVICE_OLLAMA_INFERENCE,
            SERVICE_OLLAMA_INSPECTOR,
        ]
    );

    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(transport.clone())),
        plugin,
    ];
    let mut context = compose(&plugins).unwrap();
    let catalog = context
        .get::<OllamaCatalog>(SERVICE_OLLAMA_CATALOG)
        .unwrap();
    let mounted_profile = context
        .get::<OllamaProfile>(SERVICE_OLLAMA_PROFILE)
        .unwrap();
    let inference = context
        .get::<OllamaInference>(SERVICE_OLLAMA_INFERENCE)
        .unwrap();
    let inspector = context
        .get::<OllamaInspector>(SERVICE_OLLAMA_INSPECTOR)
        .unwrap();
    assert_eq!(mounted_profile.default_model(), "gemma4");
    assert_eq!(inference.provider_profile(), profile.provider_profile());

    let readiness = inspector
        .inspect("gemma4", CancellationToken::new())
        .await
        .unwrap();
    let picker = readiness.picker_model();
    assert_eq!(picker.record().name, "gemma4");
    assert_eq!(picker.context_window(), Some(131_072));
    assert_eq!(
        picker.descriptor().capabilities.tools,
        heycode_llm::CapabilitySupport::Supported
    );
    assert_eq!(
        picker.descriptor().capabilities.image_input,
        heycode_llm::CapabilitySupport::Supported
    );
    assert_eq!(
        picker.descriptor().capabilities.reasoning,
        heycode_llm::CapabilitySupport::Unsupported
    );
    let running = picker.running().expect("fixture model is loaded");
    assert_eq!(running.context_length(), 4_096);
    assert_eq!(running.size_vram(), 5_333_539_264);

    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            OLLAMA_PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        view.snapshot.provider,
        profile.provider_profile().descriptor
    );
    assert_eq!(view.snapshot.models, vec![picker.descriptor()]);
    assert_eq!(
        catalog.picker_descriptor(),
        profile.provider_profile().descriptor
    );

    let requests = transport.requests();
    assert!(
        requests
            .iter()
            .all(|request| request.authorization.is_none())
    );
    assert!(requests.iter().all(|request| {
        !request.url.contains("/chat")
            && !request.url.contains("/pull")
            && !request.url.contains("/generate")
    }));
    let show = requests
        .iter()
        .filter(|request| request.url.ends_with("/api/show"))
        .collect::<Vec<_>>();
    assert_eq!(show.len(), 2, "inspector and catalog each enrich once");
    assert!(show.iter().all(|request| {
        request.method == heycode_http::HttpMethod::Post
            && serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()
                == serde_json::json!({"model":"gemma4","verbose":false})
    }));

    context.shutdown();
    assert!(
        models
            .refresh(
                OLLAMA_PROVIDER,
                CatalogRefreshMode::Force,
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_embedding_model_is_visible_but_withheld_from_the_chat_picker() {
    let transport = product_transport_with_show(EMBEDDING_SHOW);
    let inspector = OllamaInspector::new(service(&transport), OllamaEndpoint::local());
    let fault = inspector
        .inspect("gemma4", CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(fault, OllamaReadinessError::ModelNotChatCapable);
    assert!(!format!("{fault:?} {fault}").contains("future-provider-capability"));

    let catalog = OllamaCatalog::new(service(&transport), OllamaEndpoint::local());
    assert!(
        catalog
            .picker_models(CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(transport.requests().iter().all(|request| {
        !request.url.contains("/chat")
            && !request.url.contains("/pull")
            && !request.url.contains("/generate")
    }));
}

#[tokio::test]
async fn inference_profile_reaches_the_documented_chat_route_without_relabeling_native_evidence() {
    let transport = Arc::new(SmokeTransport::new(vec![
        smoke_event(serde_json::json!({
            "id":"chatcmpl-smoke",
            "choices":[{"index":0,"delta":{"content":"ready"},"finish_reason":null}]
        })),
        smoke_event(serde_json::json!({
            "id":"chatcmpl-smoke",
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
        })),
        Ok(SseEvent {
            event: "message".to_owned(),
            data: "[DONE]".to_owned(),
            id: None,
            retry_ms: None,
        }),
    ]));
    let inference = OllamaInference::new(
        HttpService::new(transport.clone()),
        OllamaEndpoint::local(),
        "gemma3",
    )
    .unwrap();
    assert_eq!(inference.info().name, OLLAMA_PROVIDER);
    assert_eq!(inference.info().default_model, "gemma3");
    assert!(inference.credential_reference().is_none());
    assert_eq!(
        inference.provider_profile(),
        OllamaProfile::new("gemma3").unwrap().provider_profile()
    );
    assert_eq!(
        Provider::descriptor(&inference).protocols,
        vec![ProviderProtocol::OpenAiChatCompletions]
    );
    assert!(inference.inference_adapter().is_some());

    let call = inference
        .resolve(smoke_draft(), &ModelDescriptor::unknown("gemma3"))
        .unwrap();
    let output = InferenceAdapter::stream(&inference, call)
        .collect::<Vec<_>>()
        .await;
    assert!(output.iter().all(Result::is_ok), "{output:#?}");
    assert!(
        output
            .iter()
            .any(|event| matches!(event, Ok(InferenceEvent::TextDelta(text)) if text == "ready"))
    );

    let captured = transport.captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.url, "http://localhost:11434/v1/chat/completions");
    assert_eq!(captured.authorization.as_deref(), Some("Bearer ollama"));
    assert_eq!(captured.body["model"], "gemma3");
    assert_eq!(captured.body["stream"], true);

    let native = OllamaCatalog::new(service(&self::transport()), OllamaEndpoint::local());
    assert_eq!(
        native.provider_descriptor().protocols,
        vec![ProviderProtocol::Unknown]
    );
}

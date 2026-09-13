//! Cross-boundary Gemini provider option and normalization tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceEvent, InferenceInput, InputModality,
    ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing,
    NativeFeature, Provider, ProviderOptionContext, RequestDraft, RouteCredential,
};
use heycode_provider_google::{
    CodeExecutionRequest, ExternalApiAuth, ExternalGroundingRequest,
    GOOGLE_CODE_EXECUTION_IMPLEMENTATION, GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
    GOOGLE_EXTERNAL_GROUNDING_LOGICAL, GOOGLE_GEMINI_3_7_FLASH, GOOGLE_SEARCH_IMPLEMENTATION,
    GOOGLE_VERTEX_PROVIDER, GeminiCacheRequest, GoogleGeminiProvider, GoogleSearchRequest,
};
use tokio_util::sync::CancellationToken;

struct Transport {
    events: Mutex<Option<Vec<Result<SseEvent, heycode_http::TransportError>>>>,
    body: Mutex<Option<serde_json::Value>>,
}

impl HttpTransport for Transport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        *self.body.lock().unwrap() =
            Some(serde_json::from_slice(request.body().unwrap_or_default()).unwrap());
        Box::pin(futures::stream::iter(
            self.events.lock().unwrap().take().unwrap_or_default(),
        ))
    }
}

fn chunk(value: serde_json::Value) -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: "message".to_owned(),
        data: value.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
        display_name: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(65_536),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            native_web: CapabilitySupport::Supported,
            prompt_cache: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "google".to_owned(),
        model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1),
        effective_at_ms: 2,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("latest"))],
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
async fn exact_selected_search_route_controls_option_wire_events_and_state() {
    let transport = Arc::new(Transport {
        events: Mutex::new(Some(vec![chunk(serde_json::json!({
            "responseId":"grounded-1",
            "candidates":[{
                "index":0,
                "content":{"role":"model","parts":[{"text":"answer"}]},
                "groundingMetadata":{
                    "groundingChunks":[{"web":{"uri":"https://example.test/a","title":"A"}}],
                    "groundingSupports":[{
                        "groundingChunkIndices":[0],
                        "segment":{"partIndex":0,"startIndex":0,"endIndex":6,"text":"answer"}
                    }]
                },
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}
        }))])),
        body: Mutex::new(None),
    });
    let provider = GoogleGeminiProvider::developer(
        HttpService::new(transport.clone()),
        RouteCredential::fixed("key"),
        "GEMINI_API_KEY",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap()
    .with_google_search(
        GoogleSearchRequest::web(),
        vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
    )
    .unwrap()
    .with_code_execution(
        CodeExecutionRequest::new(),
        vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
    )
    .unwrap();
    let search_route = heycode_core::NativeToolRoute::new(
        "web_search",
        GOOGLE_SEARCH_IMPLEMENTATION,
        heycode_core::NativeToolImplementationKind::Provider,
        Some("google".to_owned()),
    )
    .unwrap();
    let selected = model();
    let options = provider
        .request_options_for(ProviderOptionContext::new(
            &selected,
            std::slice::from_ref(&search_route),
        ))
        .unwrap();
    assert_eq!(options.len(), 1, "configured code must stay unselected");
    let mut request = draft();
    request.native_tool_routes = vec![search_route];
    request.native_features = vec![NativeFeature::Web];
    request.provider_options = options;
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, &selected)
        .unwrap();
    let events = provider
        .inference_adapter()
        .unwrap()
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    let body = transport.body.lock().unwrap().clone().unwrap();
    assert_eq!(body["tools"], serde_json::json!([{"googleSearch":{}}]));
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::ServerToolCall { call, .. } if call.logical() == "web_search")));
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::Citation { citation, .. } if citation.url() == "https://example.test/a")));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ProviderState(_)))
    );
    assert!(!events.iter().any(|event| matches!(event, InferenceEvent::ServerToolCall { call, .. } if call.logical() == "code_execution")));
}

#[test]
fn a_configured_but_unselected_provider_tool_never_becomes_an_option() {
    let transport = Arc::new(Transport {
        events: Mutex::new(Some(Vec::new())),
        body: Mutex::new(None),
    });
    let provider = GoogleGeminiProvider::developer(
        HttpService::new(transport),
        RouteCredential::fixed("key"),
        "GEMINI_API_KEY",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap()
    .with_code_execution(
        CodeExecutionRequest::new(),
        vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
    )
    .unwrap();
    assert!(
        provider
            .request_options_for(ProviderOptionContext::new(&model(), &[]))
            .unwrap()
            .is_empty()
    );
    let unknown_route = heycode_core::NativeToolRoute::new(
        "code_execution",
        "google:unknown-code",
        heycode_core::NativeToolImplementationKind::Provider,
        Some("google".to_owned()),
    )
    .unwrap();
    assert!(
        provider
            .request_options_for(ProviderOptionContext::new(&model(), &[unknown_route]))
            .is_err()
    );
    assert_ne!(GOOGLE_CODE_EXECUTION_IMPLEMENTATION, "google:unknown-code");
}

#[test]
fn explicit_cache_resource_kind_must_match_the_gemini_product_route() {
    let developer = GoogleGeminiProvider::developer(
        HttpService::new(Arc::new(Transport {
            events: Mutex::new(Some(Vec::new())),
            body: Mutex::new(None),
        })),
        RouteCredential::fixed("key"),
        "GEMINI_API_KEY",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap();
    assert!(
        developer
            .with_cache(
                GeminiCacheRequest::vertex_explicit(
                    "projects/vertex-fixture/locations/global/cachedContents/123456",
                )
                .unwrap()
            )
            .is_err()
    );

    let vertex = GoogleGeminiProvider::vertex(
        HttpService::new(Arc::new(Transport {
            events: Mutex::new(Some(Vec::new())),
            body: Mutex::new(None),
        })),
        "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/google",
        RouteCredential::fixed("oauth"),
        "gcp:cloud-platform-access-token",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap();
    assert!(
        vertex
            .with_cache(GeminiCacheRequest::explicit("cachedContents/cache-1").unwrap())
            .is_err()
    );
}

#[tokio::test]
async fn explicit_cache_option_and_cumulative_usage_preserve_read_and_uncached_facts() {
    let transport = Arc::new(Transport {
        events: Mutex::new(Some(vec![
            chunk(serde_json::json!({
                "responseId":"cached-1",
                "candidates":[{
                    "index":0,
                    "content":{"role":"model","parts":[{"text":"cached"}]}
                }],
                "usageMetadata":{
                    "promptTokenCount":10,
                    "cachedContentTokenCount":6,
                    "candidatesTokenCount":1
                }
            })),
            chunk(serde_json::json!({
                "responseId":"cached-1",
                "candidates":[{"index":0,"finishReason":"STOP"}],
                "usageMetadata":{"candidatesTokenCount":2}
            })),
        ])),
        body: Mutex::new(None),
    });
    let provider = GoogleGeminiProvider::developer(
        HttpService::new(transport.clone()),
        RouteCredential::fixed("key"),
        "GEMINI_API_KEY",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap()
    .with_cache(GeminiCacheRequest::explicit("cachedContents/cache-1").unwrap())
    .unwrap();
    let selected = model();
    let options = provider
        .request_options_for(ProviderOptionContext::new(&selected, &[]))
        .unwrap();
    assert_eq!(options.len(), 1);
    let mut request = draft();
    request.provider_options = options;
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, &selected)
        .unwrap();
    let events = provider
        .inference_adapter()
        .unwrap()
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();

    assert_eq!(
        transport.body.lock().unwrap().as_ref().unwrap()["cachedContent"],
        "cachedContents/cache-1"
    );
    let cache = events
        .iter()
        .find_map(|event| match event {
            InferenceEvent::ResponseMetadata(metadata) => metadata.cache_usage(),
            _ => None,
        })
        .expect("explicit cache metadata");
    assert_eq!(cache.input_tokens(), 10);
    assert_eq!(cache.cache_read_tokens(), 6);
    assert_eq!(cache.cache_write_tokens(), 0);
    assert_eq!(cache.uncached_input_tokens(), Some(4));
}

#[tokio::test]
async fn selected_code_route_normalizes_calls_results_and_lossless_parts() {
    let transport = Arc::new(Transport {
        events: Mutex::new(Some(vec![chunk(serde_json::json!({
            "responseId":"code-1",
            "candidates":[{
                "index":0,
                "content":{"role":"model","parts":[
                    {"executableCode":{"language":"PYTHON","code":"print(4)"}},
                    {"codeExecutionResult":{"outcome":"OUTCOME_OK","output":"4"}},
                    {"text":"4"}
                ]},
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}
        }))])),
        body: Mutex::new(None),
    });
    let provider = GoogleGeminiProvider::developer(
        HttpService::new(transport.clone()),
        RouteCredential::fixed("key"),
        "GEMINI_API_KEY",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap()
    .with_code_execution(
        CodeExecutionRequest::new(),
        vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
    )
    .unwrap();
    let route = heycode_core::NativeToolRoute::new(
        "code_execution",
        GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
        heycode_core::NativeToolImplementationKind::Provider,
        Some("google".to_owned()),
    )
    .unwrap();
    let selected = model();
    let options = provider
        .request_options_for(ProviderOptionContext::new(
            &selected,
            std::slice::from_ref(&route),
        ))
        .unwrap();
    let mut request = draft();
    request.native_tool_routes = vec![route];
    request.provider_options = options;
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, &selected)
        .unwrap();
    let events = provider
        .inference_adapter()
        .unwrap()
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();

    assert_eq!(
        transport.body.lock().unwrap().as_ref().unwrap()["tools"],
        serde_json::json!([{"codeExecution":{}}])
    );
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::ServerToolCall { call, .. } if call.logical() == "code_execution")));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, InferenceEvent::ServerToolResult { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        InferenceEvent::ProviderState(state)
            if state.data()["parts"][0]["executableCode"]["language"] == "PYTHON"
                && state.data()["parts"][1]["codeExecutionResult"]["output"] == "4"
    )));
}

#[tokio::test]
async fn selected_vertex_external_grounding_wires_retrieval_and_normalizes_public_citation() {
    let transport = Arc::new(Transport {
        events: Mutex::new(Some(vec![chunk(serde_json::json!({
            "responseId":"external-1",
            "candidates":[{
                "index":0,
                "content":{"role":"model","parts":[{"text":"Renew online."}]},
                "groundingMetadata":{
                    "retrievalQueries":["private query"],
                    "groundingChunks":[{"retrievedContext":{
                        "uri":"https://dmv.example/renew",
                        "title":"Renew a licence",
                        "text":"private snippet"
                    }}],
                    "groundingSupports":[{
                        "groundingChunkIndices":[0],
                        "segment":{"partIndex":0,"startIndex":0,"endIndex":13,"text":"Renew online."}
                    }]
                },
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}
        }))])),
        body: Mutex::new(None),
    });
    let provider = GoogleGeminiProvider::vertex(
        HttpService::new(transport.clone()),
        "https://aiplatform.googleapis.test/v1/projects/p/locations/global/publishers/google",
        RouteCredential::fixed("oauth"),
        "gcp:cloud-platform-access-token",
        GOOGLE_GEMINI_3_7_FLASH,
    )
    .unwrap()
    .with_external_grounding(
        ExternalGroundingRequest::simple_search(
            "https://search.example.test/query",
            ExternalApiAuth::no_auth(),
        )
        .unwrap(),
        vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()],
    )
    .unwrap();
    let route = heycode_core::NativeToolRoute::new(
        GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
        GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
        heycode_core::NativeToolImplementationKind::Provider,
        Some(GOOGLE_VERTEX_PROVIDER.to_owned()),
    )
    .unwrap();
    let selected = model();
    let options = provider
        .request_options_for(ProviderOptionContext::new(
            &selected,
            std::slice::from_ref(&route),
        ))
        .unwrap();
    let mut request = draft();
    request.provider = GOOGLE_VERTEX_PROVIDER.to_owned();
    request.native_tool_routes = vec![route];
    request.provider_options = options;
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, &selected)
        .unwrap();
    let events = provider
        .inference_adapter()
        .unwrap()
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();

    assert_eq!(
        transport.body.lock().unwrap().as_ref().unwrap()["tools"][0]["retrieval"]["externalApi"]["endpoint"],
        "https://search.example.test/query"
    );
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::ServerToolCall { call, .. } if call.logical() == GOOGLE_EXTERNAL_GROUNDING_LOGICAL)));
    assert!(events.iter().any(|event| matches!(event, InferenceEvent::Citation { citation, .. } if citation.url() == "https://dmv.example/renew")));
    let debug = format!("{events:?}");
    assert!(!debug.contains("private query"));
    assert!(!debug.contains("private snippet"));
}

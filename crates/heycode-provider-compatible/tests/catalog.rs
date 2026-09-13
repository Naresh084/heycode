//! Bounded discovery and exact model-evidence contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream,
};
use heycode_llm::{CapabilitySupport, ModelCatalog, RouteCredential};
use heycode_provider_compatible::{CompatibleCatalog, spec};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

struct Transport {
    status: u16,
    content_type: String,
    replies: Mutex<VecDeque<Vec<u8>>>,
    urls: Arc<Mutex<Vec<String>>>,
}
impl HttpTransport for Transport {
    fn send(&self, request: HttpRequest, _: CancellationToken) -> BufferedResponseFuture {
        self.urls.lock().unwrap().push(request.url().to_string());
        let body = self.replies.lock().unwrap().pop_front().unwrap();
        let status = self.status;
        let content_type = self.content_type.clone();
        Box::pin(async move {
            Ok(HttpResponse {
                status,
                content_type: Some(content_type),
                headers: BTreeMap::new(),
                body,
            })
        })
    }
    fn sse(&self, _: HttpSseRequest, _: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}
fn catalog(
    provider: &str,
    bodies: Vec<serde_json::Value>,
) -> (CompatibleCatalog, Arc<Mutex<Vec<String>>>) {
    catalog_response(provider, bodies, 200, "application/json")
}

fn catalog_response(
    provider: &str,
    bodies: Vec<serde_json::Value>,
    status: u16,
    content_type: &str,
) -> (CompatibleCatalog, Arc<Mutex<Vec<String>>>) {
    let urls = Arc::new(Mutex::new(Vec::new()));
    let transport = Transport {
        status,
        content_type: content_type.into(),
        replies: Mutex::new(
            bodies
                .into_iter()
                .map(|v| serde_json::to_vec(&v).unwrap())
                .collect(),
        ),
        urls: urls.clone(),
    };
    let http = HttpService::new(Arc::new(transport));
    (
        CompatibleCatalog::new(
            *spec(provider).unwrap(),
            http,
            "https://fixture.invalid/models",
            RouteCredential::fixed("fixture-key"),
        )
        .unwrap(),
        urls,
    )
}

#[tokio::test]
async fn fireworks_paginates_metadata_without_promoting_absent_capabilities() {
    let (catalog, urls) = catalog(
        "fireworks",
        vec![
            serde_json::json!({"models":[{"name":"accounts/fireworks/models/one","displayName":"One","state":"READY","conversationConfig":{},"supportsServerless":true,"supportsTools":true,"supportsImageInput":false,"contextLength":12345}],"nextPageToken":"a&b"}),
            serde_json::json!({"models":[{"name":"accounts/fireworks/models/two","state":"READY","conversationConfig":{},"supportsServerless":true},{"name":"accounts/fireworks/models/private","state":"READY","conversationConfig":{},"supportsServerless":false}]}),
        ],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(
        models[0].capabilities.image_input,
        CapabilitySupport::Unsupported
    );
    assert_eq!(models[1].capabilities.tools, CapabilitySupport::Unknown);
    let urls = urls.lock().unwrap();
    assert_eq!(urls.len(), 2);
    assert!(urls[1].contains("pageToken=a%26b"));
}

#[tokio::test]
async fn groq_uses_exact_documented_model_evidence_and_rejects_malformed_rows() {
    let (catalog, _) = catalog(
        "groq",
        vec![
            serde_json::json!({"data":[{"id":"openai/gpt-oss-120b","active":true},{"id":"future-model"},{"id":"removed","active":false}]}),
        ],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(models[1].capabilities.tools, CapabilitySupport::Unknown);
    let (bad, _) = self::catalog(
        "groq",
        vec![serde_json::json!({"data":[{"id":"bad\nmodel"}]})],
    );
    assert!(bad.fetch(CancellationToken::new()).await.is_err());
}

#[tokio::test]
async fn fireworks_serverless_retirement_is_not_lost_during_discovery() {
    let (catalog, _) = catalog(
        "fireworks",
        vec![serde_json::json!({"models":[{
            "name":"accounts/fireworks/models/retiring","state":"READY","conversationConfig":{},"supportsServerless":true,
            "deprecationDate":{"year":1970,"month":1,"day":2}
        }]})],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models[0].lifecycle.retirement_at_ms, Some(86_400_000));
    assert!(!models[0].lifecycle.is_selectable(86_400_000));
}

#[tokio::test]
async fn mistral_lists_only_chat_models_and_preserves_independent_capability_evidence() {
    let (catalog, _) = catalog(
        "mistral",
        vec![serde_json::json!({"data":[
            {"id":"chat","capabilities":{"completion_chat":true,"function_calling":true,"vision":false},"max_context_length":32000},
            {"id":"future-chat","capabilities":{"completion_chat":true}},
            {"id":"embedding","capabilities":{"completion_chat":false}},
            {"id":"unknown"},
            {"id":"archived","archived":true,"capabilities":{"completion_chat":true}}
        ]})],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["chat", "future-chat"]
    );
    assert_eq!(models[0].context_window, Some(32000));
    assert_eq!(models[0].capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(
        models[0].capabilities.image_input,
        CapabilitySupport::Unsupported
    );
    assert_eq!(models[1].capabilities.tools, CapabilitySupport::Unknown);
    assert_eq!(
        models[1].capabilities.image_input,
        CapabilitySupport::Unknown
    );
}

#[tokio::test]
async fn together_filters_non_chat_tasks_without_inventing_intrinsic_support() {
    let (catalog, _) = catalog(
        "together",
        vec![serde_json::json!([
            {"id":"chat-model","type":"chat","context_length":8192},
            {"id":"embed-model","type":"embedding"},
            {"id":"unknown-model"}
        ])],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "chat-model");
    assert_eq!(models[0].context_window, Some(8192));
    assert_eq!(models[0].capabilities.tools, CapabilitySupport::Unknown);
    assert_eq!(
        models[0].capabilities.image_input,
        CapabilitySupport::Unknown
    );
}

#[tokio::test]
async fn together_documented_tool_model_does_not_promote_neighboring_ids() {
    let (catalog, _) = catalog(
        "together",
        vec![serde_json::json!([
            {"id":"meta-llama/Llama-3.3-70B-Instruct-Turbo","type":"chat"},
            {"id":"meta-llama/Llama-3.3-70B-Instruct-Turbo-future","type":"chat"}
        ])],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models[0].capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(models[1].capabilities.tools, CapabilitySupport::Unknown);
}

#[tokio::test]
async fn xai_uses_language_catalog_and_only_explicit_image_evidence() {
    let (catalog, _) = catalog(
        "xai",
        vec![serde_json::json!({"models":[
            {"id":"vision-chat","input_modalities":["text","image"]},
            {"id":"text-chat","input_modalities":["text"]},
            {"id":"unknown-chat"}
        ]})],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 3);
    assert_eq!(
        models[0].capabilities.image_input,
        CapabilitySupport::Supported
    );
    assert_eq!(
        models[1].capabilities.image_input,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        models[2].capabilities.image_input,
        CapabilitySupport::Unknown
    );
    assert!(
        models
            .iter()
            .all(|model| model.capabilities.tools == CapabilitySupport::Unknown)
    );
}

#[tokio::test]
async fn all_compatible_catalogs_accept_json_media_types_and_classify_rejections() {
    use heycode_llm::CatalogFailureKind;
    let cases = [
        (
            "xai",
            serde_json::json!({"models": [{"id": "fixture-model"}]}),
        ),
        (
            "together",
            serde_json::json!([{"id": "fixture-model", "type": "chat"}]),
        ),
        (
            "mistral",
            serde_json::json!({"data": [{"id": "fixture-model", "capabilities": {"completion_chat": true}}]}),
        ),
        (
            "groq",
            serde_json::json!({"data": [{"id": "fixture-model", "active": true}]}),
        ),
        (
            "fireworks",
            serde_json::json!({"models": [{"name": "accounts/fireworks/models/fixture", "state": "READY", "supportsServerless": true, "conversationConfig": {}}]}),
        ),
    ];
    for (provider, body) in cases {
        for media_type in [
            "application/json",
            "application/json; charset=utf-8",
            "Application/JSON",
            "application/vnd.api+json",
        ] {
            let (catalog, _) = catalog_response(provider, vec![body.clone()], 200, media_type);
            assert_eq!(
                catalog.fetch(CancellationToken::new()).await.unwrap().len(),
                1,
                "{provider}: {media_type}"
            );
        }
        for (status, media_type, expected) in [
            (401, "application/json", CatalogFailureKind::Unauthorized),
            (403, "text/html", CatalogFailureKind::Unauthorized),
            (429, "application/json", CatalogFailureKind::Unavailable),
            (503, "text/html", CatalogFailureKind::Unavailable),
            (200, "text/html", CatalogFailureKind::InvalidResponse),
        ] {
            let (catalog, _) = catalog_response(provider, vec![body.clone()], status, media_type);
            assert_eq!(
                catalog
                    .fetch(CancellationToken::new())
                    .await
                    .unwrap_err()
                    .kind(),
                expected,
                "{provider}: {status}"
            );
        }
    }
}

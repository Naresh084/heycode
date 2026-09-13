//! PZA04 native Z.AI web-search result/source durability.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpTransport, SseEventStream,
    TransportError,
};
use heycode_llm::InferenceEvent;
use heycode_provider_zai::{
    ZAI_WEB_SEARCH_ENDPOINT, ZAI_WEB_SEARCH_IMPLEMENTATION, ZaiWebSearchClient, ZaiWebSearchError,
    ZaiWebSearchRecord, ZaiWebSearchRequest, zai_web_search_contribution,
};
use tokio_util::sync::CancellationToken;

struct CapturedRequest {
    url: String,
    authorization: Option<String>,
    body: serde_json::Value,
}

struct SearchTransport {
    response: Mutex<Option<Result<HttpResponse, TransportError>>>,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl SearchTransport {
    fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            response: Mutex::new(Some(Ok(HttpResponse {
                status,
                content_type: Some("application/json".to_owned()),
                headers: Default::default(),
                body: serde_json::to_vec(&body).unwrap(),
            }))),
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl HttpTransport for SearchTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.captured.lock().unwrap().push(CapturedRequest {
            url: request.url().to_owned(),
            authorization: request
                .headers()
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("authorization"))
                .map(|header| header.value().to_owned()),
            body: serde_json::from_slice(request.body().unwrap_or_default()).unwrap(),
        });
        let response = self.response.lock().unwrap().take().unwrap();
        Box::pin(async move { response })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn fixture() -> serde_json::Value {
    serde_json::json!({
        "id":"search-task-123",
        "created":1748261757,
        "search_result":[
            {
                "title":"Rust async guide",
                "content":"A concise guide to asynchronous Rust.",
                "link":"https://example.com/rust-async",
                "media":"Example Docs",
                "icon":"https://example.com/favicon.ico",
                "refer":"ref_1",
                "publish_date":"2026-08-20"
            },
            {
                "title":"Tokio tutorial",
                "content":"Runtime and task fundamentals.",
                "link":"https://example.org/tokio",
                "media":"Example Org",
                "icon":"https://example.org/icon.png",
                "refer":"ref_2",
                "publish_date":"2026-08-21"
            }
        ]
    })
}

#[tokio::test]
async fn native_search_keeps_every_source_field_and_projects_durable_events() {
    let transport = Arc::new(SearchTransport::json(200, fixture()));
    let captured = Arc::clone(&transport.captured);
    let client =
        ZaiWebSearchClient::with_key(heycode_http::HttpService::new(transport), "test-key");
    let request = ZaiWebSearchRequest::new("async rust", 2).unwrap();
    let record = client
        .search(request.clone(), CancellationToken::new())
        .await
        .unwrap();

    let sent = &captured.lock().unwrap()[0];
    assert_eq!(sent.url, ZAI_WEB_SEARCH_ENDPOINT);
    assert_eq!(sent.authorization.as_deref(), Some("Bearer test-key"));
    assert_eq!(sent.body["search_engine"], "search-prime");
    assert_eq!(sent.body["search_query"], "async rust");
    assert_eq!(sent.body["count"], 2);

    assert_eq!(record.id(), "search-task-123");
    assert_eq!(record.created(), 1_748_261_757);
    assert_eq!(record.results().len(), 2);
    let first = &record.results()[0];
    assert_eq!(first.title(), "Rust async guide");
    assert_eq!(first.summary(), "A concise guide to asynchronous Rust.");
    assert_eq!(first.url(), "https://example.com/rust-async");
    assert_eq!(first.site_name(), "Example Docs");
    assert_eq!(first.icon_url(), "https://example.com/favicon.ico");
    assert_eq!(first.reference(), "ref_1");
    assert_eq!(first.published(), "2026-08-20");

    let encoded = record.to_json().unwrap();
    assert_eq!(ZaiWebSearchRecord::from_json(&encoded).unwrap(), record);
    let mut chat_response = fixture();
    let rows = chat_response
        .as_object_mut()
        .unwrap()
        .remove("search_result")
        .unwrap();
    chat_response["web_search"] = rows;
    assert_eq!(
        ZaiWebSearchRecord::from_value(&chat_response).unwrap(),
        record
    );

    let projection = record.project(&request, 0).unwrap();
    assert_eq!(projection.metadata(), &record);
    assert!(matches!(
        &projection.events()[0],
        InferenceEvent::ServerToolCall { call, .. }
            if call.logical() == "web_search" && call.provider_name() == "web_search"
    ));
    assert!(matches!(
        &projection.events()[1],
        InferenceEvent::ServerToolResult { result, .. }
            if result.output_count() == Some(2)
                && result.sources().len() == 2
                && result.sources()[0].url() == "https://example.com/rust-async"
    ));
    let InferenceEvent::ServerToolResult { result, .. } = &projection.events()[1] else {
        panic!("the second event must be the durable search result")
    };
    let metadata = result.sources()[0].web_metadata().unwrap();
    assert_eq!(metadata.site_name(), "Example Docs");
    assert_eq!(metadata.icon_url(), "https://example.com/favicon.ico");
    assert_eq!(metadata.provider_reference(), "ref_1");
    assert_eq!(metadata.published(), "2026-08-20");
    assert_eq!(
        projection
            .events()
            .iter()
            .filter(|event| matches!(event, InferenceEvent::Citation { .. }))
            .count(),
        2
    );

    let debug = format!("{projection:?}");
    assert!(!debug.contains("https://example.com/rust-async"));
    assert!(!debug.contains("A concise guide"));

    let contribution = zai_web_search_contribution().unwrap();
    assert_eq!(contribution.route().logical(), "web_search");
    assert_eq!(
        contribution.route().implementation(),
        ZAI_WEB_SEARCH_IMPLEMENTATION
    );
    assert_eq!(contribution.route().provider(), Some("zai"));
}

#[test]
fn one_unsafe_result_rejects_the_complete_search_generation() {
    let mut response = fixture();
    response["search_result"][1]["link"] = serde_json::json!("http://user@example.org/private");
    let error = ZaiWebSearchRecord::from_value(&response).unwrap_err();
    assert!(matches!(
        error,
        ZaiWebSearchError::UnsafeResult { index: 1 }
    ));
}

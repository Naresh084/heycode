//! PAWS02 Amazon Bedrock Mantle `/v1/models` discovery.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_http::TransportError;
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, ModelCatalog, ModelDescriptor, ModelLifecycleStatus,
    ModelPricing, ProviderProtocol,
};
use heycode_provider_aws::{
    MANTLE_PROVIDER, mantle_provider_descriptor, provider_descriptor as runtime_descriptor,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    MANTLE_URL, Secret, TEST_SECRET, failing_http, mantle_body, mantle_catalog,
    mantle_catalog_over, mantle_entry, raw_response, response,
};

const GPT_OSS: &str = "openai.gpt-oss-120b";
const CLAUDE: &str = "anthropic.claude-sonnet-5";
const GLM: &str = "zai.glm-5";

async fn fetch(
    responses: Vec<heycode_http::HttpResponse>,
) -> Result<Vec<ModelDescriptor>, heycode_llm::CatalogFetchError> {
    let (context, catalog, _) = mantle_catalog(Secret::Present, responses);
    let result = catalog.fetch(CancellationToken::new()).await;
    drop(context);
    result
}

#[tokio::test]
async fn discovery_addresses_the_regional_mantle_endpoint_with_a_bearer_token() {
    let (context, catalog, requests) = mantle_catalog(
        Secret::Present,
        vec![response(200, &mantle_body(vec![mantle_entry(GPT_OSS)]))],
    );
    catalog.fetch(CancellationToken::new()).await.unwrap();
    let recorded = requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    let (url, method, headers) = &recorded[0];
    assert_eq!(url, MANTLE_URL);
    assert_eq!(method, "Get");
    assert!(
        headers.contains(&("authorization".to_owned(), format!("Bearer {TEST_SECRET}"))),
        "the Bedrock API key is a plain bearer token on this endpoint: {headers:?}"
    );
    drop(context);
}

#[tokio::test]
async fn the_generation_is_exactly_the_listed_models_and_nothing_else() {
    let models = fetch(vec![response(
        200,
        &mantle_body(vec![mantle_entry(GPT_OSS), mantle_entry(CLAUDE)]),
    )])
    .await
    .unwrap();
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        [CLAUDE, GPT_OSS],
        "the endpoint's own list is the accessible set, in id order, with nothing added"
    );
}

#[tokio::test]
async fn every_listed_model_normalizes_to_an_exact_id_and_unknown_everything_else() {
    let models = fetch(vec![response(200, &mantle_body(vec![mantle_entry(GLM)]))])
        .await
        .unwrap();
    assert_eq!(
        models,
        vec![ModelDescriptor::unknown(GLM)],
        "only `model.id` is reliable on this endpoint, so everything else stays Unknown"
    );
    let model = &models[0];
    assert_eq!(model.lifecycle.status, ModelLifecycleStatus::Unknown);
    assert_eq!(model.context_window, None);
    assert_eq!(model.max_output_tokens, None);
    assert!(model.aliases.is_empty());
    assert_eq!(model.pricing, ModelPricing::unknown());
}

#[tokio::test]
async fn wire_fields_documented_as_unreliable_cannot_reach_a_descriptor() {
    let models = fetch(vec![response(
        200,
        &mantle_body(vec![mantle_entry(GPT_OSS)]),
    )])
    .await
    .unwrap();
    let rendered = format!("{:?}", models[0]);
    for unreliable in [
        "unreliable-owner-must-not-surface",
        "Unreliable Display Name",
        "999999",
        "1700000000",
    ] {
        assert!(
            !rendered.contains(unreliable),
            "`{unreliable}` is documented as unreliable and must not be normalized: {rendered}"
        );
    }
    assert_eq!(
        models[0].display_name, GPT_OSS,
        "an id is the only honest display name this endpoint supports"
    );
}

#[tokio::test]
async fn endpoint_capability_never_becomes_a_model_capability() {
    // AWS publishes server-side tool use, web search and prompt caching for
    // the bedrock-mantle *endpoint*. None of that is evidence about any model
    // the endpoint happens to list.
    let descriptor = mantle_provider_descriptor();
    assert!(
        descriptor
            .protocols
            .contains(&ProviderProtocol::OpenAiResponses),
        "the endpoint's protocol support is a real, published fact"
    );
    let models = fetch(vec![response(
        200,
        &mantle_body(vec![mantle_entry(GPT_OSS)]),
    )])
    .await
    .unwrap();
    let capabilities = &models[0].capabilities;
    for (name, support) in [
        ("tools", capabilities.tools),
        ("native_web", capabilities.native_web),
        ("prompt_cache", capabilities.prompt_cache),
        ("reasoning", capabilities.reasoning),
        ("image_input", capabilities.image_input),
        ("document_input", capabilities.document_input),
        ("structured_output", capabilities.structured_output),
        ("native_compaction", capabilities.native_compaction),
    ] {
        assert_eq!(
            support,
            CapabilitySupport::Unknown,
            "endpoint support must not promote `{name}` for a model that never claimed it"
        );
    }
}

#[test]
fn the_mantle_provider_is_a_different_surface_from_the_runtime_provider() {
    let mantle = mantle_provider_descriptor();
    let runtime = runtime_descriptor();
    assert_ne!(mantle.id, runtime.id);
    assert_eq!(mantle.id, MANTLE_PROVIDER);
    assert_eq!(
        mantle.protocols,
        vec![
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::AnthropicMessages,
        ]
    );
    assert!(
        !mantle
            .protocols
            .contains(&ProviderProtocol::BedrockConverse),
        "Converse is documented as unavailable on bedrock-mantle"
    );
    assert!(
        !runtime
            .protocols
            .contains(&ProviderProtocol::OpenAiResponses),
        "the two descriptors must not converge into one claim"
    );
}

#[tokio::test]
async fn one_invalid_model_id_rejects_the_whole_generation() {
    for bad in [
        serde_json::json!({"id": "no-dot-separator"}),
        serde_json::json!({"id": "UPPER.case"}),
        serde_json::json!({"id": "openai..traversal"}),
        serde_json::json!({"id": ""}),
        serde_json::json!({"object": "model"}),
    ] {
        let failure = fetch(vec![response(
            200,
            &mantle_body(vec![mantle_entry(GPT_OSS), bad.clone()]),
        )])
        .await
        .unwrap_err();
        assert_eq!(
            failure.kind(),
            CatalogFailureKind::InvalidResponse,
            "{bad} must reject the generation rather than be dropped from it"
        );
    }
}

#[tokio::test]
async fn duplicate_model_ids_reject_the_whole_generation() {
    let failure = fetch(vec![response(
        200,
        &mantle_body(vec![mantle_entry(GPT_OSS), mantle_entry(GPT_OSS)]),
    )])
    .await
    .unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn an_empty_or_dataless_list_is_refused_rather_than_published_as_an_empty_catalog() {
    for body in [
        mantle_body(Vec::new()),
        serde_json::json!({ "object": "list" }),
    ] {
        let failure = fetch(vec![response(200, &body)]).await.unwrap_err();
        assert_eq!(
            failure.kind(),
            CatalogFailureKind::InvalidResponse,
            "{body} would otherwise replace a good catalog with nothing"
        );
    }
}

#[tokio::test]
async fn an_oversized_list_is_refused_rather_than_truncated() {
    let entries: Vec<serde_json::Value> = (0..2049)
        .map(|index| mantle_entry(&format!("openai.model-{index}")))
        .collect();
    let failure = fetch(vec![response(200, &mantle_body(entries))])
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn response_statuses_map_onto_the_stable_catalog_failure_classes() {
    for (status, expected) in [
        (401, CatalogFailureKind::Unauthorized),
        (403, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (500, CatalogFailureKind::Unavailable),
        (503, CatalogFailureKind::Unavailable),
        (400, CatalogFailureKind::InvalidResponse),
        (404, CatalogFailureKind::InvalidResponse),
    ] {
        let failure = fetch(vec![response(
            status,
            &serde_json::json!({"message": "provider-body-must-not-leak"}),
        )])
        .await
        .unwrap_err();
        assert_eq!(failure.kind(), expected, "HTTP {status}");
    }
}

#[tokio::test]
async fn a_parsable_body_under_a_non_json_content_type_is_still_refused() {
    // The body below is a valid model list, so the JSON parser would happily
    // publish it. Only the content-type gate can refuse it, which is what
    // makes this case pin that gate rather than the parser behind it.
    let listing = mantle_body(vec![mantle_entry(GPT_OSS)])
        .to_string()
        .into_bytes();
    for content_type in [Some("text/html"), Some("application/octet-stream"), None] {
        let failure = fetch(vec![raw_response(200, content_type, listing.clone())])
            .await
            .unwrap_err();
        assert_eq!(
            failure.kind(),
            CatalogFailureKind::InvalidResponse,
            "content type {content_type:?} must not reach the parser"
        );
    }
}

#[tokio::test]
async fn a_json_content_type_with_an_unparsable_body_is_refused_by_the_parser() {
    let failure = fetch(vec![raw_response(
        200,
        Some("application/json"),
        b"<html>captive portal</html>".to_vec(),
    )])
    .await
    .unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn transport_failures_keep_their_own_class() {
    for (error, expected) in [
        (TransportError::Timeout, CatalogFailureKind::Network),
        (
            TransportError::Network {
                message: "dns-detail-must-not-leak".to_owned(),
            },
            CatalogFailureKind::Network,
        ),
        (
            TransportError::ResponseTooLarge { max_bytes: 16 },
            CatalogFailureKind::InvalidResponse,
        ),
    ] {
        let (http, _) = failing_http(error);
        let (context, catalog) = mantle_catalog_over(Secret::Present, http);
        let failure = catalog.fetch(CancellationToken::new()).await.unwrap_err();
        assert_eq!(failure.kind(), expected);
        drop(context);
    }
}

#[tokio::test]
async fn cancellation_stops_discovery_before_any_request() {
    let (context, catalog, requests) = mantle_catalog(
        Secret::Present,
        vec![response(200, &mantle_body(vec![mantle_entry(GPT_OSS)]))],
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let failure = catalog.fetch(cancellation).await.unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::Cancelled);
    assert!(
        requests.lock().unwrap().is_empty(),
        "a cancelled refresh must not reach AWS"
    );
    drop(context);
}

#[tokio::test]
async fn an_absent_credential_is_unauthorized_and_never_reaches_the_endpoint() {
    let (context, catalog, requests) = mantle_catalog(
        Secret::Absent,
        vec![response(200, &mantle_body(vec![mantle_entry(GPT_OSS)]))],
    );
    let failure = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(failure.kind(), CatalogFailureKind::Unauthorized);
    assert!(requests.lock().unwrap().is_empty());
    drop(context);
}

//! PGCP05 Vertex external-API grounding request and citation projection.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::InferenceEvent;
use heycode_provider_google::{
    ExternalApiAuth, ExternalApiKeyLocation, ExternalGroundingError, ExternalGroundingRequest,
    ExternalGroundingSpec, GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
    GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND, GOOGLE_EXTERNAL_GROUNDING_TOOL_NAME,
    GOOGLE_VERTEX_PROVIDER, GroundingProjector, GroundingStatus,
};

#[test]
fn simple_search_uses_a_secret_reference_and_never_accepts_key_material() {
    let auth = ExternalApiAuth::api_key_secret(
        "projects/vertex-fixture/secrets/search-key/versions/latest",
        "key",
        ExternalApiKeyLocation::Query,
    )
    .unwrap();
    let request =
        ExternalGroundingRequest::simple_search("https://search.example/v0/search", auth.clone())
            .unwrap();
    assert_eq!(request.spec(), &ExternalGroundingSpec::SimpleSearch);
    assert_eq!(
        request.tool_entry(),
        serde_json::json!({
            "retrieval": {
                "externalApi": {
                    "apiSpec": "SIMPLE_SEARCH",
                    "endpoint": "https://search.example/v0/search",
                    "authConfig": {
                        "authType": "API_KEY_AUTH",
                        "apiKeyConfig": {
                            "apiKeySecret": "projects/vertex-fixture/secrets/search-key/versions/latest",
                            "name": "key",
                            "httpElementLocation": "HTTP_IN_QUERY"
                        }
                    }
                }
            }
        })
    );
    let option = request.provider_option().unwrap();
    assert_eq!(option.provider(), GOOGLE_VERTEX_PROVIDER);
    assert_eq!(option.kind(), GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND);
    assert_eq!(
        option.data(),
        &serde_json::json!({ "tool": request.tool_entry() })
    );
    assert_eq!(
        request.call_input(),
        serde_json::json!({ "api_spec": "simple_search", "auth": "api_key_secret" })
    );
    assert!(!format!("{auth:?}").contains("search-key"));
    assert!(!format!("{request:?}").contains("search.example"));
}

#[test]
fn no_auth_and_elasticsearch_have_explicit_distinct_wire_shapes() {
    let request = ExternalGroundingRequest::elastic_search(
        "https://search.example/es",
        ExternalApiAuth::no_auth(),
        "products-v2",
        "grounding-template",
        Some(25),
    )
    .unwrap();
    assert_eq!(
        request.tool_entry(),
        serde_json::json!({
            "retrieval": {
                "externalApi": {
                    "apiSpec": "ELASTIC_SEARCH",
                    "endpoint": "https://search.example/es",
                    "authConfig": { "authType": "NO_AUTH" },
                    "elasticSearchParams": {
                        "index": "products-v2",
                        "searchTemplate": "grounding-template",
                        "numHits": 25
                    }
                }
            }
        })
    );
    assert_eq!(
        request.call_input(),
        serde_json::json!({ "api_spec": "elastic_search", "auth": "none" })
    );
    let debug = format!("{request:?}");
    assert!(!debug.contains("products-v2"));
    assert!(!debug.contains("grounding-template"));
    let ExternalGroundingSpec::ElasticSearch(spec) = request.spec() else {
        panic!("expected Elasticsearch spec");
    };
    assert_eq!(spec.index(), "products-v2");
    assert_eq!(spec.search_template(), "grounding-template");
    assert_eq!(spec.num_hits(), Some(25));
}

#[test]
fn every_documented_api_key_location_has_one_exact_wire_value() {
    for (location, expected) in [
        (ExternalApiKeyLocation::Query, "HTTP_IN_QUERY"),
        (ExternalApiKeyLocation::Header, "HTTP_IN_HEADER"),
        (ExternalApiKeyLocation::Path, "HTTP_IN_PATH"),
        (ExternalApiKeyLocation::Body, "HTTP_IN_BODY"),
        (ExternalApiKeyLocation::Cookie, "HTTP_IN_COOKIE"),
    ] {
        let auth = ExternalApiAuth::api_key_secret(
            "projects/vertex-fixture/secrets/search-key/versions/7",
            "key",
            location,
        )
        .unwrap();
        let request =
            ExternalGroundingRequest::simple_search("https://search.example/v0/search", auth)
                .unwrap();
        assert_eq!(
            request.tool_entry()["retrieval"]["externalApi"]["authConfig"]["apiKeyConfig"]["httpElementLocation"],
            expected
        );
    }
}

#[test]
fn endpoints_and_secret_references_fail_closed_without_echoing_values() {
    for endpoint in [
        "http://search.example/v0/search",
        "https://user:pass@search.example/v0/search",
        "https://search.example/v0/search?key=embedded",
        "https://search.example/v0/search#fragment",
    ] {
        let error = ExternalGroundingRequest::simple_search(endpoint, ExternalApiAuth::no_auth())
            .unwrap_err();
        assert_eq!(error, ExternalGroundingError::InvalidEndpoint);
        assert!(!error.to_string().contains(endpoint));
    }

    for resource in [
        "search-key",
        "projects/vertex-fixture/secrets/search/key/versions/latest",
        "projects/vertex-fixture/secrets/search-key/versions/0",
    ] {
        let error = ExternalApiAuth::api_key_secret(resource, "key", ExternalApiKeyLocation::Query)
            .unwrap_err();
        assert_eq!(error, ExternalGroundingError::InvalidSecretReference);
        assert!(!error.to_string().contains(resource));
    }
}

#[test]
fn retrieved_context_becomes_a_public_durable_ready_citation() {
    let request = ExternalGroundingRequest::simple_search(
        "https://search.example/v0/search",
        ExternalApiAuth::no_auth(),
    )
    .unwrap();
    let mut projector = GroundingProjector::external(request);
    projector
        .observe(&serde_json::json!({
            "content": {
                "role": "model",
                "parts": [{ "text": "Renew online." }]
            },
            "groundingMetadata": {
                "retrievalQueries": ["private query that must not be durable"],
                "groundingChunks": [{
                    "retrievedContext": {
                        "uri": "https://dmv.example/renew",
                        "title": "Renew a licence",
                        "text": "private retrieved snippet"
                    }
                }],
                "groundingSupports": [{
                    "segment": {
                        "partIndex": 0,
                        "startIndex": 0,
                        "endIndex": 13,
                        "text": "Renew online."
                    },
                    "groundingChunkIndices": [0]
                }]
            }
        }))
        .unwrap();
    let projection = projector.project(4, "resp_external").unwrap();
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 1 }
    );
    let [
        InferenceEvent::ServerToolCall { call, .. },
        InferenceEvent::ServerToolResult { result, .. },
        InferenceEvent::Citation { citation, .. },
    ] = projection.events()
    else {
        panic!("external grounding must project call, result, and citation");
    };
    assert_eq!(call.logical(), GOOGLE_EXTERNAL_GROUNDING_LOGICAL);
    assert_eq!(call.provider_name(), GOOGLE_EXTERNAL_GROUNDING_TOOL_NAME);
    assert_eq!(result.call_id(), call.id());
    assert_eq!(result.sources().len(), 1);
    assert_eq!(result.sources()[0].url(), "https://dmv.example/renew");
    assert_eq!(citation.url(), "https://dmv.example/renew");
    assert_eq!(citation.cited_text(), Some("Renew online."));
    let encoded = projection
        .events()
        .iter()
        .map(|event| format!("{event:?}"))
        .collect::<String>();
    assert!(!encoded.contains("private query"));
    assert!(!encoded.contains("private retrieved snippet"));
}

#[test]
fn a_non_http_external_identifier_keeps_its_position_without_becoming_a_url() {
    let request = ExternalGroundingRequest::simple_search(
        "https://search.example/v0/search",
        ExternalApiAuth::no_auth(),
    )
    .unwrap();
    let mut projector = GroundingProjector::external(request);
    projector
        .observe(&serde_json::json!({
            "content": { "parts": [{ "text": "Internal fact." }] },
            "groundingMetadata": {
                "groundingChunks": [{
                    "retrievedContext": { "uri": "document:internal-42", "title": "Internal" }
                }],
                "groundingSupports": [{
                    "segment": { "startIndex": 0, "endIndex": 14, "text": "Internal fact." },
                    "groundingChunkIndices": [0]
                }]
            }
        }))
        .unwrap();
    let projection = projector.project(0, "resp_external").unwrap();
    assert_eq!(
        projection.status(),
        GroundingStatus::Grounded { sources: 0 }
    );
    assert_eq!(projection.events().len(), 2);
}

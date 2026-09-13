//! PAWS03 runtime `ListFoundationModels` discovery and normalization.
//!
//! Fixtures mirror the documented `ListFoundationModelsResponse` shape:
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_ListFoundationModels.html>
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelLifecycle.html>

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_http::{HttpRequest, HttpResponse, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, ModelCatalog, ModelLifecycleStatus,
};
use heycode_provider_aws::{
    BEDROCK_PROVIDER, BedrockFoundationModel, BedrockInferenceType, BedrockLifecycleStatus,
    BedrockModality, BedrockRequestAuthorizer, provider_descriptor,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    EXPECTED_URL, Secret, TEST_REGION, TEST_SECRET, body, build_catalog, catalog, catalog_over,
    failing_http, http, raw_response, response, summary,
};

const MODEL_ID: &str = "anthropic.claude-sonnet-4-20250514-v1:0";

async fn discover_ok(fixture: &serde_json::Value) -> Vec<BedrockFoundationModel> {
    let (_context, catalog, _requests) = catalog(Secret::Present, vec![response(200, fixture)]);
    catalog.discover(CancellationToken::new()).await.unwrap()
}

async fn discover_err(fixture: HttpResponse) -> CatalogFetchError {
    let (_context, catalog, _requests) = catalog(Secret::Present, vec![fixture]);
    catalog
        .discover(CancellationToken::new())
        .await
        .expect_err("the generation must be refused")
}

async fn refuse_body(fixture: &serde_json::Value) -> CatalogFetchError {
    let failure = discover_err(response(200, fixture)).await;
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
    failure
}

fn only(rows: &[BedrockFoundationModel]) -> &BedrockFoundationModel {
    assert_eq!(rows.len(), 1, "fixture publishes exactly one row");
    &rows[0]
}

fn row<'a>(rows: &'a [BedrockFoundationModel], id: &str) -> &'a BedrockFoundationModel {
    rows.iter()
        .find(|row| row.id().as_str() == id)
        .unwrap_or_else(|| panic!("`{id}` must be published"))
}

// ---------------------------------------------------------------------------
// request shape and authorization seam
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovery_reads_the_documented_regional_control_plane_endpoint() {
    let (_context, catalog, requests) = catalog(
        Secret::Present,
        vec![response(200, &body(vec![summary(MODEL_ID)]))],
    );
    catalog.discover(CancellationToken::new()).await.unwrap();
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let (url, method, headers) = &recorded[0];
    assert_eq!(url, EXPECTED_URL);
    assert_eq!(method, "Get");
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "accept" && value == "application/json"),
        "discovery must ask for JSON"
    );
}

#[tokio::test]
async fn the_api_key_authorizer_attaches_exactly_one_bearer_header() {
    let (_context, catalog, requests) = catalog(
        Secret::Present,
        vec![response(200, &body(vec![summary(MODEL_ID)]))],
    );
    catalog.discover(CancellationToken::new()).await.unwrap();
    let recorded = requests.lock().unwrap();
    let (_url, _method, headers) = &recorded[0];
    let bearer: Vec<&(String, String)> = headers
        .iter()
        .filter(|(name, _)| name == "authorization")
        .collect();
    assert_eq!(bearer.len(), 1, "exactly one authorization header");
    assert_eq!(bearer[0].1, format!("Bearer {TEST_SECRET}"));
    assert!(
        headers
            .iter()
            .all(|(name, value)| name == "authorization" || !value.contains(TEST_SECRET)),
        "no other header may carry the credential"
    );
}

#[tokio::test]
async fn a_draft_region_uses_that_exact_control_plane_and_operation_key() {
    let (_context, catalog, requests) = catalog(
        Secret::Absent,
        vec![response(200, &body(vec![summary(MODEL_ID)]))],
    );
    let parameters =
        std::collections::BTreeMap::from([("region".to_owned(), "ap-southeast-2".to_owned())]);
    let models = catalog
        .fetch_parameters_with_credential(
            &parameters,
            Some(&heycode_credentials::CredentialSecret::new(TEST_SECRET)),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].0,
        "https://bedrock.ap-southeast-2.amazonaws.com/foundation-models"
    );
    assert!(
        recorded[0]
            .2
            .iter()
            .any(|(name, value)| name == "authorization"
                && value == &format!("Bearer {TEST_SECRET}"))
    );
}

#[tokio::test]
async fn draft_region_discovery_rejects_missing_or_extra_coordinates_before_http() {
    let (_context, catalog, requests) = catalog(Secret::Present, Vec::new());
    for parameters in [
        std::collections::BTreeMap::new(),
        std::collections::BTreeMap::from([
            ("region".to_owned(), TEST_REGION.to_owned()),
            ("project".to_owned(), "wrong-cloud".to_owned()),
        ]),
        std::collections::BTreeMap::from([("region".to_owned(), "US-EAST-1".to_owned())]),
    ] {
        let error = catalog
            .fetch_parameters(&parameters, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    }
    assert!(requests.lock().unwrap().is_empty());
}

/// The seam a SigV4 signer plugs into: the catalog hands over an unauthorized
/// request and publishes whatever the authorizer returns.
#[tokio::test]
async fn an_injected_authorizer_owns_every_credential_header() {
    struct StubSigner;

    #[async_trait]
    impl BedrockRequestAuthorizer for StubSigner {
        async fn authorize(
            &self,
            request: HttpRequest,
            _cancellation: CancellationToken,
        ) -> Result<HttpRequest, CatalogFetchError> {
            Ok(request
                .header("authorization", "AWS4-HMAC-SHA256 Credential=stub")
                .unwrap()
                .header("x-amz-date", "20260829T000000Z")
                .unwrap())
        }
    }

    let (service, requests) = http(vec![response(200, &body(vec![summary(MODEL_ID)]))]);
    let catalog = build_catalog(service, Arc::new(StubSigner));
    catalog.discover(CancellationToken::new()).await.unwrap();
    let recorded = requests.lock().unwrap();
    let (_url, _method, headers) = &recorded[0];
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "authorization" && value.starts_with("AWS4-HMAC-SHA256")),
        "the authorizer decides the scheme, not the catalog"
    );
    assert!(headers.iter().any(|(name, _)| name == "x-amz-date"));
}

#[tokio::test]
async fn an_absent_credential_fails_unauthorized_before_any_request() {
    let (_context, catalog, requests) = catalog(Secret::Absent, Vec::new());
    let failure = catalog
        .discover(CancellationToken::new())
        .await
        .expect_err("no credential means no discovery");
    assert_eq!(failure.kind(), CatalogFailureKind::Unauthorized);
    assert!(requests.lock().unwrap().is_empty(), "nothing may be sent");
}

#[tokio::test]
async fn a_credential_store_failure_is_unavailable_rather_than_unauthorized() {
    let (_context, catalog, _requests) = catalog(Secret::Broken, Vec::new());
    let failure = catalog
        .discover(CancellationToken::new())
        .await
        .expect_err("a store that cannot answer is not a rejection");
    assert_eq!(failure.kind(), CatalogFailureKind::Unavailable);
}

#[tokio::test]
async fn no_discovery_diagnostic_repeats_the_credential() {
    let cases: Vec<CatalogFetchError> = vec![
        discover_err(response(401, &serde_json::json!({}))).await,
        discover_err(response(500, &serde_json::json!({}))).await,
        discover_err(raw_response(200, Some("text/html"), b"<html/>".to_vec())).await,
        refuse_body(&body(Vec::new())).await,
    ];
    for failure in cases {
        assert!(
            !failure.message().contains(TEST_SECRET),
            "`{}` leaked the credential",
            failure.message()
        );
    }
}

#[tokio::test]
async fn cancellation_before_the_request_reports_the_cancelled_class() {
    let (_context, catalog, requests) = catalog(Secret::Present, Vec::new());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let failure = catalog
        .discover(cancellation)
        .await
        .expect_err("a cancelled refresh returns no catalog");
    assert_eq!(failure.kind(), CatalogFailureKind::Cancelled);
    assert!(requests.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// retained metadata: lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn active_and_legacy_lifecycles_stay_distinguishable_after_normalization() {
    let mut active = summary("amazon.nova-pro-v1:0");
    active["modelLifecycle"] = serde_json::json!({ "status": "ACTIVE" });
    let mut legacy = summary("amazon.titan-text-express-v1");
    legacy["modelLifecycle"] = serde_json::json!({ "status": "LEGACY" });
    let rows = discover_ok(&body(vec![active, legacy])).await;

    let nova = row(&rows, "amazon.nova-pro-v1:0");
    assert_eq!(nova.lifecycle().status, BedrockLifecycleStatus::Active);
    assert_eq!(
        nova.descriptor().lifecycle.status,
        ModelLifecycleStatus::Stable
    );

    let titan = row(&rows, "amazon.titan-text-express-v1");
    assert_eq!(titan.lifecycle().status, BedrockLifecycleStatus::Legacy);
    assert_eq!(
        titan.descriptor().lifecycle.status,
        ModelLifecycleStatus::Deprecated
    );
}

#[tokio::test]
async fn every_published_lifecycle_instant_is_retained() {
    let mut row = summary(MODEL_ID);
    row["modelLifecycle"] = serde_json::json!({
        "status": "LEGACY",
        "startOfLifeTime": "2024-05-13T00:00:00Z",
        "legacyTime": "2026-01-01T00:00:00Z",
        "publicExtendedAccessTime": "2026-06-01T00:00:00Z",
        "endOfLifeTime": "2026-12-01T00:00:00Z"
    });
    let rows = discover_ok(&body(vec![row])).await;
    let lifecycle = only(&rows).lifecycle();
    assert_eq!(lifecycle.start_of_life_at_ms, Some(1_715_558_400_000));
    assert_eq!(lifecycle.legacy_at_ms, Some(1_767_225_600_000));
    assert_eq!(
        lifecycle.public_extended_access_at_ms,
        Some(1_780_272_000_000)
    );
    assert_eq!(lifecycle.end_of_life_at_ms, Some(1_796_083_200_000));
    assert_eq!(
        only(&rows).descriptor().lifecycle.retirement_at_ms,
        Some(1_796_083_200_000),
        "the end-of-life instant must reach the shared vocabulary"
    );
}

#[tokio::test]
async fn an_unrecognized_lifecycle_status_is_never_promoted_to_a_phase() {
    let mut row = summary(MODEL_ID);
    row["modelLifecycle"] = serde_json::json!({ "status": "PREVIEW" });
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(
        only(&rows).lifecycle().status,
        BedrockLifecycleStatus::Unknown
    );
    assert_eq!(
        only(&rows).descriptor().lifecycle.status,
        ModelLifecycleStatus::Unknown
    );
}

#[tokio::test]
async fn an_absent_lifecycle_object_normalizes_to_unknown_evidence() {
    let mut row = summary(MODEL_ID);
    row.as_object_mut().unwrap().remove("modelLifecycle");
    let rows = discover_ok(&body(vec![row])).await;
    let lifecycle = only(&rows).lifecycle();
    assert_eq!(lifecycle.status, BedrockLifecycleStatus::Unknown);
    assert_eq!(lifecycle.end_of_life_at_ms, None);
    assert_eq!(
        only(&rows).descriptor().lifecycle.status,
        ModelLifecycleStatus::Unknown
    );
}

/// A published deadline outranks a cached phase: an `ACTIVE` row whose end of
/// life has passed must read as retired rather than as current.
#[tokio::test]
async fn a_passed_end_of_life_instant_retires_a_still_active_row() {
    let mut row = summary(MODEL_ID);
    row["modelLifecycle"] = serde_json::json!({
        "status": "ACTIVE",
        "endOfLifeTime": "2026-01-01T00:00:00Z"
    });
    let rows = discover_ok(&body(vec![row])).await;
    let lifecycle = &only(&rows).descriptor().lifecycle;
    assert_eq!(lifecycle.status, ModelLifecycleStatus::Stable);
    assert_eq!(
        lifecycle.effective_status(1_767_225_600_001),
        ModelLifecycleStatus::Retired
    );
    assert!(lifecycle.is_selectable(1_767_225_599_999));
}

// ---------------------------------------------------------------------------
// retained metadata: modalities
// ---------------------------------------------------------------------------

#[tokio::test]
async fn input_and_output_modalities_are_retained_as_separate_lists() {
    let mut row = summary("amazon.titan-image-generator-v2:0");
    row["inputModalities"] = serde_json::json!(["TEXT", "IMAGE"]);
    row["outputModalities"] = serde_json::json!(["IMAGE"]);
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(
        only(&rows).input_modalities(),
        Some([BedrockModality::Text, BedrockModality::Image].as_slice())
    );
    assert_eq!(
        only(&rows).output_modalities(),
        Some([BedrockModality::Image].as_slice()),
        "output modalities must not be merged into the input list"
    );
}

#[tokio::test]
async fn an_unpublished_modality_list_is_not_an_empty_one() {
    let mut row = summary(MODEL_ID);
    row.as_object_mut().unwrap().remove("inputModalities");
    row["outputModalities"] = serde_json::json!([]);
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(only(&rows).input_modalities(), None);
    assert_eq!(only(&rows).output_modalities(), Some([].as_slice()));
}

#[tokio::test]
async fn an_image_input_modality_is_the_only_capability_this_endpoint_proves() {
    let rows = discover_ok(&body(vec![summary(MODEL_ID)])).await;
    let capabilities = &only(&rows).descriptor().capabilities;
    assert_eq!(capabilities.image_input, CapabilitySupport::Supported);
    assert_eq!(
        capabilities.document_input,
        CapabilitySupport::Unknown,
        "the modality enumeration cannot express documents, so absence proves nothing"
    );
}

#[tokio::test]
async fn a_text_only_input_list_is_explicit_negative_image_evidence() {
    let mut row = summary(MODEL_ID);
    row["inputModalities"] = serde_json::json!(["TEXT"]);
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(
        only(&rows).descriptor().capabilities.image_input,
        CapabilitySupport::Unsupported
    );
}

#[tokio::test]
async fn an_unpublished_input_list_leaves_image_input_unknown() {
    let mut row = summary(MODEL_ID);
    row.as_object_mut().unwrap().remove("inputModalities");
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(
        only(&rows).descriptor().capabilities.image_input,
        CapabilitySupport::Unknown
    );
}

// ---------------------------------------------------------------------------
// retained metadata: streaming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn response_streaming_is_retained_as_tri_state_evidence() {
    for (published, expected) in [
        (Some(true), CapabilitySupport::Supported),
        (Some(false), CapabilitySupport::Unsupported),
        (None, CapabilitySupport::Unknown),
    ] {
        let mut row = summary(MODEL_ID);
        match published {
            Some(value) => row["responseStreamingSupported"] = serde_json::json!(value),
            None => {
                row.as_object_mut()
                    .unwrap()
                    .remove("responseStreamingSupported");
            }
        }
        let rows = discover_ok(&body(vec![row])).await;
        assert_eq!(
            only(&rows).response_streaming(),
            expected,
            "streaming evidence for {published:?} must be {expected:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// retained metadata: inference types
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inference_types_are_retained_and_decide_on_demand_callability() {
    let mut on_demand = summary("amazon.nova-lite-v1:0");
    on_demand["inferenceTypesSupported"] = serde_json::json!(["ON_DEMAND", "PROVISIONED"]);
    let mut provisioned = summary("amazon.titan-text-premier-v1:0");
    provisioned["inferenceTypesSupported"] = serde_json::json!(["PROVISIONED"]);
    let rows = discover_ok(&body(vec![on_demand, provisioned])).await;

    let nova = row(&rows, "amazon.nova-lite-v1:0");
    assert_eq!(
        nova.inference_types(),
        Some(
            [
                BedrockInferenceType::OnDemand,
                BedrockInferenceType::Provisioned
            ]
            .as_slice()
        )
    );
    assert_eq!(nova.on_demand(), CapabilitySupport::Supported);

    let titan = row(&rows, "amazon.titan-text-premier-v1:0");
    assert_eq!(
        titan.inference_types(),
        Some([BedrockInferenceType::Provisioned].as_slice())
    );
    assert_eq!(
        titan.on_demand(),
        CapabilitySupport::Unsupported,
        "a provisioned-only model is not callable on demand"
    );
}

#[tokio::test]
async fn an_unpublished_inference_type_list_leaves_on_demand_unknown() {
    let mut row = summary(MODEL_ID);
    row.as_object_mut()
        .unwrap()
        .remove("inferenceTypesSupported");
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(only(&rows).inference_types(), None);
    assert_eq!(only(&rows).on_demand(), CapabilitySupport::Unknown);
}

// ---------------------------------------------------------------------------
// conservative projection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unproven_capabilities_are_never_promoted_to_supported() {
    let rows = discover_ok(&body(vec![summary(MODEL_ID)])).await;
    let capabilities = &only(&rows).descriptor().capabilities;
    for (label, support) in [
        ("tools", capabilities.tools),
        ("reasoning", capabilities.reasoning),
        ("document_input", capabilities.document_input),
        ("structured_output", capabilities.structured_output),
        ("native_web", capabilities.native_web),
        ("native_compaction", capabilities.native_compaction),
        ("prompt_cache", capabilities.prompt_cache),
    ] {
        assert_eq!(
            support,
            CapabilitySupport::Unknown,
            "`{label}` has no evidence on this endpoint and must stay Unknown"
        );
    }
}

#[tokio::test]
async fn limits_pricing_and_performance_stay_absent_when_none_are_published() {
    let rows = discover_ok(&body(vec![summary(MODEL_ID)])).await;
    let descriptor = only(&rows).descriptor();
    assert_eq!(descriptor.context_window, None);
    assert_eq!(descriptor.max_output_tokens, None);
    assert!(descriptor.aliases.is_empty());
    assert!(
        descriptor.pricing.is_unknown(),
        "unknown cost is not free cost"
    );
    assert!(descriptor.performance.is_unknown());
}

#[tokio::test]
async fn the_model_name_becomes_the_display_name_and_falls_back_to_the_model_id() {
    let mut named = summary("amazon.nova-pro-v1:0");
    named["modelName"] = serde_json::json!("  Nova Pro  ");
    let mut unnamed = summary("amazon.titan-text-lite-v1");
    unnamed.as_object_mut().unwrap().remove("modelName");
    let rows = discover_ok(&body(vec![named, unnamed])).await;
    assert_eq!(
        row(&rows, "amazon.nova-pro-v1:0").descriptor().display_name,
        "Nova Pro"
    );
    assert_eq!(
        row(&rows, "amazon.titan-text-lite-v1")
            .descriptor()
            .display_name,
        "amazon.titan-text-lite-v1"
    );
}

#[tokio::test]
async fn the_provider_name_and_arn_are_retained_next_to_the_descriptor() {
    let rows = discover_ok(&body(vec![summary(MODEL_ID)])).await;
    assert_eq!(only(&rows).provider_name(), Some("Anthropic"));
    assert_eq!(
        only(&rows).arn(),
        format!("arn:aws:bedrock:{TEST_REGION}::foundation-model/{MODEL_ID}")
    );
}

#[tokio::test]
async fn the_catalog_projection_publishes_one_id_sorted_descriptor_per_row() {
    let (_context, catalog, _requests) = catalog(
        Secret::Present,
        vec![response(
            200,
            &body(vec![
                summary("meta.llama3-70b-instruct-v1:0"),
                summary("amazon.nova-pro-v1:0"),
            ]),
        )],
    );
    let descriptors = catalog.fetch(CancellationToken::new()).await.unwrap();
    let ids: Vec<&str> = descriptors.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["amazon.nova-pro-v1:0", "meta.llama3-70b-instruct-v1:0"]
    );
    assert_eq!(provider_descriptor().id, BEDROCK_PROVIDER);
}

// ---------------------------------------------------------------------------
// all-or-nothing rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_malformed_row_rejects_the_whole_generation() {
    let mut broken = summary("amazon.nova-pro-v1:0");
    broken["modelId"] = serde_json::json!("amazon.nova-pro/../../secrets");
    let failure = refuse_body(&body(vec![summary(MODEL_ID), broken])).await;
    assert!(failure.message().contains("invalid model id"), "{failure}");
}

#[tokio::test]
async fn an_invalid_model_arn_rejects_the_whole_generation() {
    let mut broken = summary(MODEL_ID);
    broken["modelArn"] = serde_json::json!("arn:aws:s3:us-east-1::bucket/whatever");
    let failure = refuse_body(&body(vec![broken])).await;
    assert!(failure.message().contains("invalid model ARN"), "{failure}");
}

#[tokio::test]
async fn a_display_name_carrying_a_control_character_rejects_the_generation() {
    let mut broken = summary(MODEL_ID);
    broken["modelName"] = serde_json::json!("Nova\u{7}Pro");
    let failure = refuse_body(&body(vec![broken])).await;
    assert!(
        failure.message().contains("invalid model name"),
        "{failure}"
    );
}

#[tokio::test]
async fn duplicate_model_ids_reject_the_whole_generation() {
    let failure = refuse_body(&body(vec![summary(MODEL_ID), summary(MODEL_ID)])).await;
    assert!(failure.message().contains("duplicate"), "{failure}");
}

#[tokio::test]
async fn an_unrecognized_modality_member_rejects_the_whole_generation() {
    let mut broken = summary(MODEL_ID);
    broken["outputModalities"] = serde_json::json!(["TEXT", "AUDIO"]);
    let failure = refuse_body(&body(vec![broken])).await;
    assert!(
        failure.message().contains("unrecognized modality"),
        "{failure}"
    );
}

#[tokio::test]
async fn an_unrecognized_inference_type_rejects_the_whole_generation() {
    let mut broken = summary(MODEL_ID);
    broken["inferenceTypesSupported"] = serde_json::json!(["INFERENCE_PROFILE"]);
    let failure = refuse_body(&body(vec![broken])).await;
    assert!(
        failure.message().contains("unrecognized inference type"),
        "{failure}"
    );
}

#[tokio::test]
async fn a_lifecycle_object_without_the_required_status_rejects_the_generation() {
    let mut broken = summary(MODEL_ID);
    broken["modelLifecycle"] = serde_json::json!({ "endOfLifeTime": "2026-12-01T00:00:00Z" });
    refuse_body(&body(vec![broken])).await;
}

#[tokio::test]
async fn an_unparsable_lifecycle_instant_rejects_the_generation() {
    let mut broken = summary(MODEL_ID);
    broken["modelLifecycle"] =
        serde_json::json!({ "status": "ACTIVE", "endOfLifeTime": "2026-12-01" });
    let failure = refuse_body(&body(vec![broken])).await;
    assert!(
        failure.message().contains("invalid lifecycle instant"),
        "{failure}"
    );
}

#[tokio::test]
async fn an_empty_model_list_is_refused_rather_than_published() {
    let failure = refuse_body(&body(Vec::new())).await;
    assert!(failure.message().contains("empty"), "{failure}");
}

#[tokio::test]
async fn a_response_without_model_summaries_is_refused() {
    let failure = refuse_body(&serde_json::json!({})).await;
    assert!(
        failure.message().contains("no model summaries"),
        "{failure}"
    );
}

#[tokio::test]
async fn an_unknown_response_member_does_not_take_the_catalog_down() {
    let mut row = summary(MODEL_ID);
    row["someFutureField"] = serde_json::json!({ "nested": true });
    let rows = discover_ok(&body(vec![row])).await;
    assert_eq!(only(&rows).id().as_str(), MODEL_ID);
}

// ---------------------------------------------------------------------------
// transport-level refusal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_non_json_response_is_refused() {
    let failure = discover_err(raw_response(
        200,
        Some("text/html"),
        b"<html>sign in</html>".to_vec(),
    ))
    .await;
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
    assert!(failure.message().contains("not JSON"), "{failure}");
}

/// An oversized payload is refused, never truncated into a shorter model list
/// that would read as "these models do not exist in this region".
#[tokio::test]
async fn an_oversized_response_body_is_refused_before_normalization() {
    let oversized = vec![b'{'; 2 * 1024 * 1024 + 1];
    let failure = discover_err(raw_response(200, Some("application/json"), oversized)).await;
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
    assert!(failure.message().contains("too large"), "{failure}");
}

#[tokio::test]
async fn http_status_classes_map_to_stable_catalog_failure_kinds() {
    for (status, expected) in [
        (401_u16, CatalogFailureKind::Unauthorized),
        (403, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (500, CatalogFailureKind::Unavailable),
        (503, CatalogFailureKind::Unavailable),
        (400, CatalogFailureKind::InvalidResponse),
        (302, CatalogFailureKind::InvalidResponse),
    ] {
        let failure = discover_err(response(status, &serde_json::json!({}))).await;
        assert_eq!(failure.kind(), expected, "status {status}");
    }
}

#[tokio::test]
async fn a_transport_size_refusal_is_an_invalid_response_not_a_network_fault() {
    let (service, _requests) = failing_http(TransportError::ResponseTooLarge { max_bytes: 16 });
    let (_context, catalog) = catalog_over(Secret::Present, service);
    let failure = catalog
        .discover(CancellationToken::new())
        .await
        .expect_err("an oversized transport response is refused");
    assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn a_transport_timeout_preserves_the_network_class() {
    let (service, _requests) = failing_http(TransportError::Timeout);
    let (_context, catalog) = catalog_over(Secret::Present, service);
    let failure = catalog
        .discover(CancellationToken::new())
        .await
        .expect_err("a timeout is not a catalog");
    assert_eq!(failure.kind(), CatalogFailureKind::Network);
}

/// The row cap bounds a hostile response; it never truncates a real one into a
/// shorter model list.
#[tokio::test]
async fn a_model_list_past_the_row_cap_is_refused_rather_than_truncated() {
    let summaries: Vec<serde_json::Value> = (0..=2048)
        .map(|index| summary(&format!("vendor.model-{index}-v1:0")))
        .collect();
    let failure = refuse_body(&body(summaries)).await;
    assert!(failure.message().contains("too large"), "{failure}");
}

#[tokio::test]
async fn an_enumeration_list_past_its_member_cap_is_refused() {
    let mut row = summary(MODEL_ID);
    row["inputModalities"] = serde_json::json!(vec!["TEXT"; 65]);
    let failure = refuse_body(&body(vec![row])).await;
    assert!(
        failure.message().contains("oversized modality list"),
        "{failure}"
    );
}

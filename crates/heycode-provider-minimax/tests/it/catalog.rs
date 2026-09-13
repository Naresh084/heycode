//! PMM02: both documented list endpoints normalize MiniMax's current models.

use std::collections::BTreeMap;
use std::sync::Arc;

use heycode_core::Context;
use heycode_credentials::CredentialsService;
use heycode_http::TransportError;
use heycode_llm::{CapabilitySupport, CatalogFailureKind, ModelCatalog, ModelLifecycleStatus};
use heycode_provider_minimax::{
    MINIMAX_M3, MiniMaxApiFamily, MiniMaxCatalog, MiniMaxCatalogConfig, MiniMaxPlanId,
    MiniMaxProfile, MiniMaxRegion, PayAsYouGo, TokenPlan, documented_model_ids,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::support::{Reply, ScriptedTransport, credentials_holding, http_from, ok, reply};

const PAYG_SECRET: &str = "pmm02-pay-as-you-go-not-a-real-key";
const TOKEN_PLAN_SECRET: &str = "sk-cp-pmm02-not-a-real-key";

fn payg_store(context: &Context) -> Arc<CredentialsService> {
    credentials_holding(context, &[("MINIMAX_API_KEY", "api-key", PAYG_SECRET)])
}

fn token_plan_store(context: &Context) -> Arc<CredentialsService> {
    credentials_holding(
        context,
        &[(
            "MINIMAX_TOKEN_PLAN_KEY",
            "subscription-key",
            TOKEN_PLAN_SECRET,
        )],
    )
}

fn openai_body() -> serde_json::Value {
    json!({
        "object": "list",
        "data": [
            {"id": "MiniMax-M3", "object": "model", "created": 1_780_272_000_i64, "owned_by": "minimax"},
            {"id": "MiniMax-M2.7", "object": "model", "created": 1_773_799_200_i64, "owned_by": "minimax"},
            {"id": "MiniMax-M2.5", "object": "model", "created": 1_770_948_000_i64, "owned_by": "minimax"}
        ]
    })
}

fn anthropic_body() -> serde_json::Value {
    json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "2026-06-01T00:00:00Z", "display_name": "MiniMax-M3", "type": "model"},
            {"id": "MiniMax-M2.7", "created_at": "2026-03-18T02:00:00Z", "display_name": "MiniMax-M2.7", "type": "model"},
            {"id": "MiniMax-M2.5", "created_at": "2026-02-13T02:00:00Z", "display_name": "MiniMax-M2.5", "type": "model"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": false,
        "last_id": "MiniMax-M2.5"
    })
}

fn payg_catalog(
    context: &Context,
    family: MiniMaxApiFamily,
    replies: Vec<Reply>,
) -> (MiniMaxCatalog<PayAsYouGo>, Arc<ScriptedTransport>) {
    let transport = ScriptedTransport::new(replies);
    let catalog = MiniMaxCatalog::new(
        http_from(transport.clone()),
        payg_store(context),
        MiniMaxCatalogConfig::new(MiniMaxProfile::<PayAsYouGo>::international(), family),
    )
    .expect("documented international route builds");
    (catalog, transport)
}

#[tokio::test]
async fn the_openai_compatible_list_normalizes_the_documented_models() {
    let context = Context::new();
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::OpenAiCompatible,
        vec![ok(openai_body())],
    );

    let models = catalog
        .fetch(CancellationToken::new())
        .await
        .expect("documented list normalizes");
    let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["MiniMax-M2.5", "MiniMax-M2.7", "MiniMax-M3"]);
    let m3 = models
        .iter()
        .find(|model| model.id == MINIMAX_M3)
        .expect("M3 present");
    assert_eq!(m3.context_window, Some(1_000_000));
    assert_eq!(m3.display_name, "MiniMax-M3");
}

#[tokio::test]
async fn the_anthropic_compatible_list_normalizes_the_documented_models() {
    let context = Context::new();
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(anthropic_body())],
    );

    let models = catalog
        .fetch(CancellationToken::new())
        .await
        .expect("documented list normalizes");
    let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["MiniMax-M2.5", "MiniMax-M2.7", "MiniMax-M3"]);
}

#[tokio::test]
async fn both_list_endpoints_normalize_the_same_models_identically() {
    let context = Context::new();
    let (openai, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::OpenAiCompatible,
        vec![ok(openai_body())],
    );
    let (anthropic, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(anthropic_body())],
    );

    let from_openai = openai.fetch(CancellationToken::new()).await.unwrap();
    let from_anthropic = anthropic.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(
        from_openai, from_anthropic,
        "the two dialects disagree about the same models"
    );
}

#[tokio::test]
async fn a_documented_model_takes_its_display_name_from_the_table_not_the_wire() {
    let context = Context::new();
    // The Anthropic list publishes a display name the OpenAI list cannot. If
    // the wire name won for a documented id, the two dialects would drift the
    // moment MiniMax prettified one of them.
    let renamed = json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "2026-06-01T00:00:00Z", "display_name": "MiniMax M3 (Frontier)", "type": "model"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": false,
        "last_id": "MiniMax-M3"
    });
    let openai_single = json!({
        "object": "list",
        "data": [{"id": "MiniMax-M3", "object": "model", "created": 1_780_272_000_u64, "owned_by": "minimax"}]
    });
    let (anthropic, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(renamed)],
    );
    let (openai, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::OpenAiCompatible,
        vec![ok(openai_single)],
    );

    let from_anthropic = anthropic.fetch(CancellationToken::new()).await.unwrap();
    let from_openai = openai.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(from_anthropic[0].display_name, "MiniMax-M3");
    assert_eq!(
        from_anthropic, from_openai,
        "a renamed documented model made the two dialects disagree"
    );
}

#[tokio::test]
async fn each_list_endpoint_sends_the_authentication_scheme_its_reference_page_documents() {
    let context = Context::new();
    let (openai, openai_transport) = payg_catalog(
        &context,
        MiniMaxApiFamily::OpenAiCompatible,
        vec![ok(openai_body())],
    );
    let (anthropic, anthropic_transport) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(anthropic_body())],
    );
    openai.fetch(CancellationToken::new()).await.unwrap();
    anthropic.fetch(CancellationToken::new()).await.unwrap();

    let (openai_url, openai_headers) = openai_transport.requests().remove(0);
    assert_eq!(openai_url, "https://api.minimax.io/v1/models");
    assert_eq!(
        openai_headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {PAYG_SECRET}").as_str())
    );
    assert!(!openai_headers.contains_key("x-api-key"));

    let (anthropic_url, anthropic_headers) = anthropic_transport.requests().remove(0);
    assert_eq!(anthropic_url, "https://api.minimax.io/anthropic/v1/models");
    assert_eq!(
        anthropic_headers.get("x-api-key").map(String::as_str),
        Some(PAYG_SECRET)
    );
    assert!(!anthropic_headers.contains_key("authorization"));
}

#[tokio::test]
async fn discovery_spends_the_credential_bound_to_the_profile_plan() {
    let context = Context::new();
    let transport = ScriptedTransport::new(vec![ok(anthropic_body())]);
    let catalog = MiniMaxCatalog::new(
        http_from(transport.clone()),
        token_plan_store(&context),
        MiniMaxCatalogConfig::new(
            MiniMaxProfile::<TokenPlan>::international(),
            MiniMaxApiFamily::AnthropicCompatible,
        ),
    )
    .unwrap();

    catalog.fetch(CancellationToken::new()).await.unwrap();
    let (_, headers) = transport.requests().remove(0);
    assert_eq!(
        headers.get("x-api-key").map(String::as_str),
        Some(TOKEN_PLAN_SECRET)
    );
    assert_eq!(
        catalog.provider().id,
        MiniMaxPlanId::TokenPlan.registry_name()
    );
}

#[tokio::test]
async fn a_wrong_plan_credential_fails_discovery_before_any_request() {
    let context = Context::new();
    // A Token Plan key parked under the pay-as-you-go reference.
    let credentials = credentials_holding(
        &context,
        &[("MINIMAX_API_KEY", "api-key", TOKEN_PLAN_SECRET)],
    );
    let transport = ScriptedTransport::new(Vec::new());
    let catalog = MiniMaxCatalog::new(
        http_from(transport.clone()),
        credentials,
        MiniMaxCatalogConfig::new(
            MiniMaxProfile::<PayAsYouGo>::international(),
            MiniMaxApiFamily::OpenAiCompatible,
        ),
    )
    .unwrap();

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
    assert!(
        error.message().contains("Token Plan"),
        "the failure must name the confused product: {}",
        error.message()
    );
    assert!(
        !error.message().contains("pmm02"),
        "the secret leaked: {}",
        error.message()
    );
    assert!(
        transport.requests().is_empty(),
        "an unusable credential must not reach the network"
    );
}

#[tokio::test]
async fn the_anthropic_list_walks_its_documented_cursor_to_completion() {
    let context = Context::new();
    let first = json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "2026-06-01T00:00:00Z", "display_name": "MiniMax-M3", "type": "model"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": true,
        "last_id": "MiniMax-M3"
    });
    let second = json!({
        "data": [
            {"id": "MiniMax-M2", "created_at": "2025-10-27T02:00:00Z", "display_name": "MiniMax-M2", "type": "model"}
        ],
        "first_id": "MiniMax-M2",
        "has_more": false,
        "last_id": "MiniMax-M2"
    });
    let (catalog, transport) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(first), ok(second)],
    );

    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["MiniMax-M2", "MiniMax-M3"]);
    let urls: Vec<_> = transport
        .requests()
        .into_iter()
        .map(|(url, _)| url)
        .collect();
    assert_eq!(
        urls,
        [
            "https://api.minimax.io/anthropic/v1/models".to_owned(),
            "https://api.minimax.io/anthropic/v1/models?after_id=MiniMax-M3".to_owned(),
        ]
    );
}

#[tokio::test]
async fn a_promised_page_without_a_cursor_fails_the_generation() {
    let context = Context::new();
    let page = json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "2026-06-01T00:00:00Z", "display_name": "MiniMax-M3", "type": "model"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": true,
        "last_id": null
    });
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(page)],
    );

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(error.message().contains("without a cursor"));
}

#[tokio::test]
async fn a_repeated_pagination_cursor_fails_instead_of_looping() {
    let context = Context::new();
    // Distinct rows each page, so the duplicate-id guard cannot fire first and
    // only the cursor-repeat guard stands between this and an endless walk.
    let first = json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "2026-06-01T00:00:00Z", "display_name": "MiniMax-M3", "type": "model"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": true,
        "last_id": "MiniMax-M3"
    });
    let second = json!({
        "data": [
            {"id": "MiniMax-M2", "created_at": "2025-10-27T02:00:00Z", "display_name": "MiniMax-M2", "type": "model"}
        ],
        "first_id": "MiniMax-M2",
        "has_more": true,
        "last_id": "MiniMax-M3"
    });
    let (catalog, transport) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(first), ok(second)],
    );

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(
        error.message().contains("repeated its pagination cursor"),
        "{}",
        error.message()
    );
    assert_eq!(
        transport.requests().len(),
        2,
        "the walk must stop at the repeat, not keep paging"
    );
}

#[tokio::test]
async fn a_foreign_owner_on_the_openai_list_fails_the_whole_generation() {
    let context = Context::new();
    let body = json!({
        "object": "list",
        "data": [
            {"id": "MiniMax-M3", "object": "model", "created": 1, "owned_by": "minimax"},
            {"id": "someone-else", "object": "model", "created": 2, "owned_by": "not-minimax"}
        ]
    });
    let (catalog, _) = payg_catalog(&context, MiniMaxApiFamily::OpenAiCompatible, vec![ok(body)]);

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
}

#[tokio::test]
async fn duplicate_model_ids_fail_the_whole_generation() {
    let context = Context::new();
    let body = json!({
        "object": "list",
        "data": [
            {"id": "MiniMax-M3", "object": "model", "created": 1, "owned_by": "minimax"},
            {"id": "MiniMax-M3", "object": "model", "created": 2, "owned_by": "minimax"}
        ]
    });
    let (catalog, _) = payg_catalog(&context, MiniMaxApiFamily::OpenAiCompatible, vec![ok(body)]);

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert!(error.message().contains("duplicate model ids"));
}

#[tokio::test]
async fn a_blank_or_padded_model_id_fails_the_whole_generation() {
    for bad in ["", " MiniMax-M3", "MiniMax-M3 "] {
        let context = Context::new();
        let body = json!({
            "object": "list",
            "data": [{"id": bad, "object": "model", "created": 1, "owned_by": "minimax"}]
        });
        let (catalog, _) =
            payg_catalog(&context, MiniMaxApiFamily::OpenAiCompatible, vec![ok(body)]);
        let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
        assert_eq!(
            error.kind(),
            CatalogFailureKind::InvalidResponse,
            "`{bad}` was admitted"
        );
    }
}

#[tokio::test]
async fn an_openai_envelope_that_is_not_a_list_fails_the_generation() {
    let context = Context::new();
    let body = json!({
        "object": "error",
        "data": [{"id": "MiniMax-M3", "object": "model", "created": 1, "owned_by": "minimax"}]
    });
    let (catalog, _) = payg_catalog(&context, MiniMaxApiFamily::OpenAiCompatible, vec![ok(body)]);

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(
        error.message().contains("not `list`"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn an_anthropic_row_that_is_not_a_model_fails_the_generation() {
    let context = Context::new();
    let body = json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "2026-06-01T00:00:00Z", "display_name": "MiniMax-M3", "type": "deployment"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": false,
        "last_id": "MiniMax-M3"
    });
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(body)],
    );

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(error.message().contains("malformed row"));
}

#[tokio::test]
async fn an_anthropic_row_without_a_creation_instant_fails_the_generation() {
    let context = Context::new();
    let body = json!({
        "data": [
            {"id": "MiniMax-M3", "created_at": "   ", "display_name": "MiniMax-M3", "type": "model"}
        ],
        "first_id": "MiniMax-M3",
        "has_more": false,
        "last_id": "MiniMax-M3"
    });
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(body)],
    );

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(error.message().contains("malformed row"));
}

#[tokio::test]
async fn an_empty_model_list_fails_rather_than_emptying_the_catalog() {
    let context = Context::new();
    let body = json!({"object": "list", "data": []});
    let (catalog, _) = payg_catalog(&context, MiniMaxApiFamily::OpenAiCompatible, vec![ok(body)]);

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert!(error.message().contains("empty"));
}

#[tokio::test]
async fn a_non_json_success_response_is_refused() {
    let context = Context::new();
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::OpenAiCompatible,
        vec![reply(200, Some("text/html"), "<html>login</html>")],
    );

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(error.message().contains("not JSON"));
}

#[tokio::test]
async fn http_statuses_map_onto_the_stable_catalog_failure_classes() {
    for (status, expected) in [
        (401, CatalogFailureKind::Unauthorized),
        (403, CatalogFailureKind::Unauthorized),
        (429, CatalogFailureKind::Unavailable),
        (503, CatalogFailureKind::Unavailable),
        (418, CatalogFailureKind::InvalidResponse),
    ] {
        let context = Context::new();
        let (catalog, _) = payg_catalog(
            &context,
            MiniMaxApiFamily::OpenAiCompatible,
            vec![reply(status, Some("application/json"), "{}")],
        );
        let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind(), expected, "status {status} misclassified");
    }
}

#[tokio::test]
async fn a_transport_timeout_is_reported_as_a_network_failure() {
    let context = Context::new();
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::OpenAiCompatible,
        vec![Reply::Failure(TransportError::Timeout)],
    );

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Network);
}

#[tokio::test]
async fn an_already_cancelled_refresh_never_resolves_a_credential() {
    let context = Context::new();
    let transport = ScriptedTransport::new(Vec::new());
    let catalog = MiniMaxCatalog::new(
        http_from(transport.clone()),
        payg_store(&context),
        MiniMaxCatalogConfig::new(
            MiniMaxProfile::<PayAsYouGo>::international(),
            MiniMaxApiFamily::OpenAiCompatible,
        ),
    )
    .unwrap();

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = catalog.fetch(cancellation).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Cancelled);
    assert!(transport.requests().is_empty());
}

#[test]
fn an_undocumented_region_and_dialect_pair_refuses_to_build_a_plausible_url() {
    let context = Context::new();
    let error = MiniMaxCatalog::new(
        http_from(ScriptedTransport::new(Vec::new())),
        payg_store(&context),
        MiniMaxCatalogConfig::new(
            MiniMaxProfile::<PayAsYouGo>::new(MiniMaxRegion::MainlandChina),
            MiniMaxApiFamily::OpenAiCompatible,
        ),
    )
    .expect_err("mainland-China OpenAI-compatible discovery is not documented");
    assert_eq!(error.kind(), CatalogFailureKind::InvalidResponse);
    assert!(error.message().contains("does not document"));
}

#[test]
fn the_documented_mainland_china_dialect_still_builds() {
    let context = Context::new();
    MiniMaxCatalog::new(
        http_from(ScriptedTransport::new(Vec::new())),
        payg_store(&context),
        MiniMaxCatalogConfig::new(
            MiniMaxProfile::<PayAsYouGo>::new(MiniMaxRegion::MainlandChina),
            MiniMaxApiFamily::AnthropicCompatible,
        ),
    )
    .expect("mainland-China Anthropic-compatible discovery is documented");
}

#[tokio::test]
async fn an_undocumented_model_id_keeps_its_identity_and_claims_nothing_else() {
    let context = Context::new();
    let body = json!({
        "object": "list",
        "data": [{"id": "MiniMax-Future-9", "object": "model", "created": 1, "owned_by": "minimax"}]
    });
    let (catalog, _) = payg_catalog(&context, MiniMaxApiFamily::OpenAiCompatible, vec![ok(body)]);

    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    let model = &models[0];
    assert_eq!(model.id, "MiniMax-Future-9");
    assert_eq!(model.context_window, None);
    assert_eq!(model.max_output_tokens, None);
    assert_eq!(model.capabilities.image_input, CapabilitySupport::Unknown);
    assert!(model.pricing.is_unknown());
}

#[tokio::test]
async fn an_undocumented_model_keeps_the_display_name_the_anthropic_list_publishes() {
    let context = Context::new();
    let body = json!({
        "data": [
            {"id": "MiniMax-Future-9", "created_at": "2027-01-01T00:00:00Z", "display_name": "MiniMax Future 9", "type": "model"}
        ],
        "first_id": "MiniMax-Future-9",
        "has_more": false,
        "last_id": "MiniMax-Future-9"
    });
    let (catalog, _) = payg_catalog(
        &context,
        MiniMaxApiFamily::AnthropicCompatible,
        vec![ok(body)],
    );

    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models[0].display_name, "MiniMax Future 9");
}

#[test]
fn no_documented_model_claims_a_lifecycle_or_a_price_minimax_never_published() {
    for id in documented_model_ids() {
        let model = heycode_provider_minimax::normalize_model(id.to_owned(), None);
        assert_eq!(
            model.lifecycle.effective_status(u64::MAX),
            ModelLifecycleStatus::Unknown,
            "{id} claims a lifecycle MiniMax does not publish"
        );
        assert!(model.pricing.is_unknown(), "{id} claims a price");
        assert_eq!(
            model.max_output_tokens, None,
            "{id} claims an output cap MiniMax does not publish"
        );
    }
}

#[test]
fn only_minimax_m3_claims_documented_image_input() {
    for id in documented_model_ids() {
        let model = heycode_provider_minimax::normalize_model(id.to_owned(), None);
        let expected = if id == MINIMAX_M3 {
            CapabilitySupport::Supported
        } else {
            CapabilitySupport::Unknown
        };
        assert_eq!(model.capabilities.image_input, expected, "{id}");
    }
}

#[test]
fn endpoint_level_request_parameters_never_become_model_capability_evidence() {
    for id in documented_model_ids() {
        let model = heycode_provider_minimax::normalize_model(id.to_owned(), None);
        for (name, support) in [
            ("tools", model.capabilities.tools),
            ("reasoning", model.capabilities.reasoning),
            ("structured_output", model.capabilities.structured_output),
            ("prompt_cache", model.capabilities.prompt_cache),
            ("native_web", model.capabilities.native_web),
            ("native_compaction", model.capabilities.native_compaction),
            ("document_input", model.capabilities.document_input),
        ] {
            assert_eq!(
                support,
                CapabilitySupport::Unknown,
                "{id} claims {name} from protocol evidence alone"
            );
        }
    }
}

#[test]
fn every_documented_model_carries_a_context_window() {
    let windows: BTreeMap<_, _> = documented_model_ids()
        .map(|id| {
            (
                id,
                heycode_provider_minimax::normalize_model(id.to_owned(), None).context_window,
            )
        })
        .collect();
    assert_eq!(windows.get(MINIMAX_M3), Some(&Some(1_000_000)));
    assert_eq!(windows.get("MiniMax-M2"), Some(&Some(204_800)));
    assert!(
        windows.values().all(Option::is_some),
        "a documented model lost its context window"
    );
}

#[test]
fn each_plan_owns_a_distinct_catalog_plugin_name() {
    assert_eq!(
        MiniMaxPlanId::PayAsYouGo.catalog_plugin_name(),
        "catalog-minimax"
    );
    assert_eq!(
        MiniMaxPlanId::TokenPlan.catalog_plugin_name(),
        "catalog-minimax-token-plan"
    );
    assert_ne!(
        MiniMaxPlanId::PayAsYouGo.catalog_plugin_name(),
        MiniMaxPlanId::TokenPlan.catalog_plugin_name()
    );
}

#[test]
fn the_catalog_plugin_name_constant_matches_its_runtime_plan() {
    assert_eq!(
        <PayAsYouGo as heycode_provider_minimax::MiniMaxPlan>::CATALOG_PLUGIN_NAME,
        MiniMaxPlanId::PayAsYouGo.catalog_plugin_name()
    );
    assert_eq!(
        <TokenPlan as heycode_provider_minimax::MiniMaxPlan>::CATALOG_PLUGIN_NAME,
        MiniMaxPlanId::TokenPlan.catalog_plugin_name()
    );
}

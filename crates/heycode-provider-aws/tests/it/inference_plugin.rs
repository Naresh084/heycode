//! Effect-owned AWS inference provider construction and dialect admission.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use heycode_authorization_aws::AwsRegion;
use heycode_core::{ContributionKind, CoreError, Plugin, compose};
use heycode_llm::{
    CallPurpose, CatalogRefreshMode, CatalogRegistry, ChatMessage, InferenceInput, InputModality,
    LlmSelection, ModelDescriptor, ProviderErrorClass, ProviderOptionContext, ProviderRegistry,
    RequestDraft, SERVICE_MODELS, SERVICE_PROVIDERS, llm_plugin, model_catalog_plugin,
};
use heycode_provider_aws::{
    AwsInferencePluginConfig, BEDROCK_PROVIDER, BedrockConverseModelEvidence,
    BedrockConverseModelFact, MANTLE_PROVIDER, MantleProtocol, MantleProtocolModelEvidence,
    aws_inference_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    CredentialsPlugin, Secret, body, catalog, http, http_plugin, mantle_body, mantle_entry, query,
    response, summary,
};

const CONVERSE_MODEL: &str = "anthropic.claude-sonnet-4-20250514-v1:0";
const RESPONSES_MODEL: &str = "openai.gpt-5.6-sol";
const MESSAGES_MODEL: &str = "anthropic.claude-sonnet-5";

fn base_plugins() -> Vec<Box<dyn Plugin>> {
    let (http, _) = http(Vec::new());
    vec![
        http_plugin(http),
        Box::new(CredentialsPlugin(Secret::Present)),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: BEDROCK_PROVIDER.to_owned(),
                model: CONVERSE_MODEL.to_owned(),
            },
            Vec::new(),
        ),
    ]
}

async fn converse_evidence() -> BedrockConverseModelEvidence {
    let (_owner, catalog, _) = catalog(
        Secret::Present,
        vec![response(200, &body(vec![summary(CONVERSE_MODEL)]))],
    );
    let rows = catalog.discover(CancellationToken::new()).await.unwrap();
    BedrockConverseModelEvidence::from_discovery(rows).unwrap()
}

#[tokio::test]
async fn lazy_converse_activation_composes_without_io_and_requires_live_provider_evidence() {
    let generation = body(vec![summary(CONVERSE_MODEL)]);
    let (http, requests) = http(vec![response(200, &generation)]);
    let config = AwsInferencePluginConfig::converse_live(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        CONVERSE_MODEL,
        None,
    )
    .unwrap();
    let plugins = vec![
        http_plugin(http),
        Box::new(CredentialsPlugin(Secret::Present)) as Box<dyn Plugin>,
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: BEDROCK_PROVIDER.to_owned(),
                model: CONVERSE_MODEL.to_owned(),
            },
            Vec::new(),
        ),
        aws_inference_plugin(config),
    ];
    let mut context = compose(&plugins).unwrap();
    assert!(
        requests.lock().unwrap().is_empty(),
        "composition must not discover models or call inference"
    );

    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let provider = providers.get(BEDROCK_PROVIDER).unwrap();
    let adapter = provider.inference_adapter().unwrap();
    let unknown = ModelDescriptor::unknown(CONVERSE_MODEL);
    let error = adapter
        .resolve(draft(BEDROCK_PROVIDER, CONVERSE_MODEL), &unknown)
        .expect_err("no caller assertion can replace provider-owned evidence");
    assert!(format!("{error}").contains("evidence"), "{error}");
    assert!(requests.lock().unwrap().is_empty());

    let catalogs = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let live = catalogs
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
    let selected = live
        .snapshot
        .resolve_model(CONVERSE_MODEL, 2)
        .unwrap()
        .descriptor;
    let prepared = provider
        .prepare_inference(
            ProviderOptionContext::new(&selected, &[]),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .expect("live Converse preparation returns an operation snapshot");
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(
        prepared
            .inference_adapter()
            .unwrap()
            .resolve(draft(BEDROCK_PROVIDER, CONVERSE_MODEL), &selected)
            .is_ok()
    );

    context.shutdown();
    assert!(providers.get(BEDROCK_PROVIDER).is_none());
    assert!(catalogs.cached(BEDROCK_PROVIDER).is_err());
}

fn lazy_converse_plugins(http: heycode_http::HttpService) -> Vec<Box<dyn Plugin>> {
    lazy_converse_plugins_with_ttl(http, Duration::from_secs(300))
}

fn lazy_converse_plugins_with_ttl(
    http: heycode_http::HttpService,
    ttl: Duration,
) -> Vec<Box<dyn Plugin>> {
    let config = AwsInferencePluginConfig::converse_live(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        CONVERSE_MODEL,
        None,
    )
    .unwrap();
    vec![
        http_plugin(http),
        Box::new(CredentialsPlugin(Secret::Present)),
        model_catalog_plugin(ttl),
        llm_plugin(
            LlmSelection {
                provider_name: BEDROCK_PROVIDER.to_owned(),
                model: CONVERSE_MODEL.to_owned(),
            },
            Vec::new(),
        ),
        aws_inference_plugin(config),
    ]
}

#[tokio::test]
async fn live_preparation_rejects_a_stale_cache_when_the_forced_refresh_fails() {
    let generation = body(vec![summary(CONVERSE_MODEL)]);
    let (http, requests) = http(vec![
        response(200, &generation),
        response(503, &serde_json::json!({"message":"must not surface"})),
    ]);
    let plugins = lazy_converse_plugins_with_ttl(http, Duration::ZERO);
    let mut context = compose(&plugins).unwrap();
    let catalogs = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let initial = catalogs
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let selected = initial
        .snapshot
        .resolve_model(CONVERSE_MODEL, 2)
        .unwrap()
        .descriptor;
    let provider = context
        .get::<ProviderRegistry>(SERVICE_PROVIDERS)
        .unwrap()
        .get(BEDROCK_PROVIDER)
        .unwrap();
    assert!(
        provider
            .prepare_inference(
                ProviderOptionContext::new(&selected, &[]),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .is_some()
    );
    let stale = catalogs
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(stale.warning.is_some());
    let error = provider
        .prepare_inference(
            ProviderOptionContext::new(&selected, &[]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("forced preparation must not downgrade to stale evidence");
    assert_eq!(error.class(), ProviderErrorClass::Overloaded);
    assert!(catalogs.cached(BEDROCK_PROVIDER).is_ok());
    assert_eq!(requests.lock().unwrap().len(), 2);
    context.shutdown();
}

#[tokio::test]
async fn live_preparation_honors_pre_cancel_without_resolving_another_credential() {
    let generation = body(vec![summary(CONVERSE_MODEL)]);
    let (http, requests) = http(vec![response(200, &generation)]);
    let plugins = lazy_converse_plugins(http);
    let mut context = compose(&plugins).unwrap();
    let catalogs = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let initial = catalogs
        .refresh(
            BEDROCK_PROVIDER,
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let selected = initial
        .snapshot
        .resolve_model(CONVERSE_MODEL, 2)
        .unwrap()
        .descriptor;
    let provider = context
        .get::<ProviderRegistry>(SERVICE_PROVIDERS)
        .unwrap()
        .get(BEDROCK_PROVIDER)
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = provider
        .prepare_inference(ProviderOptionContext::new(&selected, &[]), cancellation)
        .await
        .err()
        .expect("cancelled preparation must settle as cancelled");
    assert_eq!(error.class(), ProviderErrorClass::Cancelled);
    assert_eq!(requests.lock().unwrap().len(), 1);
    context.shutdown();
}

fn draft(provider: &str, model: &str) -> RequestDraft {
    RequestDraft {
        provider: provider.to_owned(),
        model: model.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1),
        effective_at_ms: 2,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
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
async fn converse_provider_inventory_and_registry_row_are_effect_owned() {
    let config = AwsInferencePluginConfig::converse(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        CONVERSE_MODEL,
        converse_evidence().await,
        None,
    )
    .unwrap();
    let mut plugins = base_plugins();
    plugins.push(aws_inference_plugin(config));
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    assert_eq!(registry.names(), vec![BEDROCK_PROVIDER]);
    let profile = registry.profiles().pop().unwrap();
    assert_eq!(profile.default_model, CONVERSE_MODEL);
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(query().reference.as_str())
    );
    assert_eq!(
        profile.descriptor.protocols,
        [heycode_core::ProviderProtocol::BedrockConverse]
    );
    let snapshot = context.plugin_inventory().snapshot().unwrap();
    assert!(snapshot.contributions.iter().any(|row| {
        row.plugin == "inference-bedrock-converse"
            && row.kind == ContributionKind::InferenceProvider
            && row.name == BEDROCK_PROVIDER
    }));
    let provider = registry.get(BEDROCK_PROVIDER).unwrap();
    let selected = provider.describe_model(CONVERSE_MODEL);
    assert!(
        provider
            .inference_adapter()
            .unwrap()
            .resolve(draft(BEDROCK_PROVIDER, CONVERSE_MODEL), &selected)
            .is_ok()
    );
    let unevidenced = ModelDescriptor::unknown("anthropic.unevidenced");
    assert!(
        provider
            .inference_adapter()
            .unwrap()
            .resolve(
                draft(BEDROCK_PROVIDER, "anthropic.unevidenced"),
                &unevidenced,
            )
            .is_err()
    );
    assert!(
        provider
            .prepare_inference(
                ProviderOptionContext::new(&selected, &[]),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .is_none(),
        "supplied evidence providers remain immediately ready"
    );

    context.shutdown();
    assert!(registry.get(BEDROCK_PROVIDER).is_none());
}

#[test]
fn mantle_responses_and_messages_are_mutually_exclusive_at_inventory_load() {
    let responses = AwsInferencePluginConfig::mantle_responses_live(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        RESPONSES_MODEL,
    )
    .unwrap();
    let messages = AwsInferencePluginConfig::mantle_messages_live(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        MESSAGES_MODEL,
        Some(128_000),
    )
    .unwrap();
    let mut plugins = base_plugins();
    plugins.push(aws_inference_plugin(responses));
    plugins.push(aws_inference_plugin(messages));
    let error = match compose(&plugins) {
        Ok(_) => panic!("one Mantle provider id cannot own two dialects"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CoreError::DuplicateContribution { ref kind, ref name, .. }
            if kind == "inference_provider" && name == MANTLE_PROVIDER
    ));
}

async fn assert_lazy_mantle_activation(
    config: AwsInferencePluginConfig,
    model: &str,
    protocol: heycode_core::ProviderProtocol,
    expected_after_catalog: bool,
) {
    let generation = mantle_body(vec![mantle_entry(model)]);
    let (http, requests) = http(vec![response(200, &generation)]);
    let plugins = vec![
        http_plugin(http),
        Box::new(CredentialsPlugin(Secret::Present)) as Box<dyn Plugin>,
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: MANTLE_PROVIDER.to_owned(),
                model: model.to_owned(),
            },
            Vec::new(),
        ),
        aws_inference_plugin(config),
    ];
    let mut context = compose(&plugins).unwrap();
    assert!(requests.lock().unwrap().is_empty());
    let providers = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    let provider = providers.get(MANTLE_PROVIDER).unwrap();
    let adapter = provider.inference_adapter().unwrap();
    assert_eq!(adapter.descriptor().protocols, [protocol]);
    assert!(
        adapter
            .resolve(
                draft(MANTLE_PROVIDER, model),
                &ModelDescriptor::unknown(model)
            )
            .is_err(),
        "a caller-created descriptor is not account/protocol evidence"
    );
    assert!(requests.lock().unwrap().is_empty());

    let catalogs = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let live = catalogs
        .refresh(
            MANTLE_PROVIDER,
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let selected = live.snapshot.resolve_model(model, 2).unwrap().descriptor;
    let preparation = provider
        .prepare_inference(
            ProviderOptionContext::new(&selected, &[]),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(preparation.is_ok(), expected_after_catalog);
    assert_eq!(
        adapter
            .resolve(draft(MANTLE_PROVIDER, model), &selected)
            .is_ok(),
        expected_after_catalog,
        "account membership must not promote unmaintained protocol evidence"
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    context.shutdown();
}

#[tokio::test]
async fn lazy_mantle_variants_require_account_and_protocol_evidence_without_composition_io() {
    assert_lazy_mantle_activation(
        AwsInferencePluginConfig::mantle_responses_live(
            AwsRegion::new("us-east-1").unwrap(),
            query(),
            RESPONSES_MODEL,
        )
        .unwrap(),
        RESPONSES_MODEL,
        heycode_core::ProviderProtocol::OpenAiResponses,
        true,
    )
    .await;
    assert_lazy_mantle_activation(
        AwsInferencePluginConfig::mantle_messages_live(
            AwsRegion::new("us-east-1").unwrap(),
            query(),
            MESSAGES_MODEL,
            Some(128_000),
        )
        .unwrap(),
        MESSAGES_MODEL,
        heycode_core::ProviderProtocol::AnthropicMessages,
        true,
    )
    .await;
    let unclassified = "anthropic.unclassified-model";
    assert_lazy_mantle_activation(
        AwsInferencePluginConfig::mantle_messages_live(
            AwsRegion::new("us-east-1").unwrap(),
            query(),
            unclassified,
            Some(128_000),
        )
        .unwrap(),
        unclassified,
        heycode_core::ProviderProtocol::AnthropicMessages,
        false,
    )
    .await;
}

#[tokio::test]
async fn converse_and_one_exact_mantle_dialect_can_coexist() {
    let converse = AwsInferencePluginConfig::converse(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        CONVERSE_MODEL,
        converse_evidence().await,
        None,
    )
    .unwrap();
    let responses = AwsInferencePluginConfig::mantle_responses(
        AwsRegion::new("us-east-1").unwrap(),
        query(),
        RESPONSES_MODEL,
        MantleProtocolModelEvidence::new(
            MantleProtocol::Responses,
            vec![RESPONSES_MODEL.to_owned()],
        )
        .unwrap(),
    )
    .unwrap();
    let mut plugins = base_plugins();
    plugins.push(aws_inference_plugin(converse));
    plugins.push(aws_inference_plugin(responses));
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<ProviderRegistry>(SERVICE_PROVIDERS).unwrap();
    assert_eq!(registry.names(), vec![BEDROCK_PROVIDER, MANTLE_PROVIDER]);
    assert_eq!(
        registry
            .get(MANTLE_PROVIDER)
            .unwrap()
            .inference_adapter()
            .unwrap()
            .descriptor()
            .protocols,
        [heycode_core::ProviderProtocol::OpenAiResponses]
    );
    context.shutdown();
    assert!(registry.names().is_empty());
}

#[test]
fn mantle_model_evidence_is_protocol_exact_and_default_bound() {
    assert!(
        BedrockConverseModelFact::new(
            ModelDescriptor::unknown(CONVERSE_MODEL),
            heycode_llm::CapabilitySupport::Unknown,
            heycode_llm::CapabilitySupport::Supported,
        )
        .is_err()
    );
    assert!(
        MantleProtocolModelEvidence::new(
            MantleProtocol::Responses,
            vec![MESSAGES_MODEL.to_owned()],
        )
        .is_err()
    );
    let evidence = MantleProtocolModelEvidence::new(
        MantleProtocol::Responses,
        vec![RESPONSES_MODEL.to_owned()],
    )
    .unwrap();
    assert!(
        AwsInferencePluginConfig::mantle_responses(
            AwsRegion::new("us-east-1").unwrap(),
            query(),
            "openai.unproven-model",
            evidence,
        )
        .is_err()
    );
}

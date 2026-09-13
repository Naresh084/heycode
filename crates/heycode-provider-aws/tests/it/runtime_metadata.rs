//! PAWS06 provider-owned Bedrock runtime request metadata.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_authorization_aws::AwsRegion;
use heycode_credentials::CredentialSecret;
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceInput, InputModality, ModelCapabilities,
    ModelDescriptor, ModelLifecycle, ModelPerformance, ModelPricing, Provider,
    ProviderOptionContext, RequestDraft, ResolveError,
};
use heycode_provider_aws::{
    BEDROCK_PROVIDER, BedrockCachePlacement, BedrockCachePoint, BedrockCacheTtl,
    BedrockConverseProvider, BedrockCrossRegionScope, BedrockGuardrailConfig,
    BedrockGuardrailStreamMode, BedrockGuardrailTrace, BedrockInferenceTargetKind,
    BedrockMetadataErrorClass, BedrockPromptCacheCapabilities, BedrockPromptCacheConfig,
    BedrockRouteMetadata, BedrockRuntimeRequestMetadata,
};

use super::support::{TEST_REGION, TEST_SECRET, http};

const FOUNDATION_MODEL: &str = "anthropic.claude-sonnet-4-6-v1";

fn region() -> AwsRegion {
    AwsRegion::new(TEST_REGION).unwrap()
}

fn descriptor(id: &str, prompt_cache: CapabilitySupport) -> ModelDescriptor {
    ModelDescriptor {
        id: id.to_owned(),
        display_name: id.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            prompt_cache,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn cache_capabilities(
    id: &str,
    placements: Vec<BedrockCachePlacement>,
    max_checkpoints: usize,
    one_hour_ttl: CapabilitySupport,
) -> BedrockPromptCacheCapabilities {
    BedrockPromptCacheCapabilities::new(id, placements, max_checkpoints, one_hour_ttl).unwrap()
}

#[test]
fn cache_points_follow_the_documented_tools_system_messages_order() {
    let config = BedrockPromptCacheConfig::new(
        vec![
            BedrockCachePoint::new(
                BedrockCachePlacement::System,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
            BedrockCachePoint::new(BedrockCachePlacement::Tools, BedrockCacheTtl::OneHour),
            BedrockCachePoint::new(
                BedrockCachePlacement::LatestUserMessage,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
        ],
        cache_capabilities(
            FOUNDATION_MODEL,
            vec![
                BedrockCachePlacement::Tools,
                BedrockCachePlacement::System,
                BedrockCachePlacement::LatestUserMessage,
            ],
            3,
            CapabilitySupport::Supported,
        ),
    )
    .unwrap();
    assert_eq!(
        config
            .points()
            .iter()
            .map(|point| point.placement())
            .collect::<Vec<_>>(),
        vec![
            BedrockCachePlacement::Tools,
            BedrockCachePlacement::System,
            BedrockCachePlacement::LatestUserMessage,
        ]
    );
}

#[test]
fn a_long_ttl_cannot_follow_a_short_ttl_in_bedrocks_processing_order() {
    let error = BedrockPromptCacheConfig::new(
        vec![
            BedrockCachePoint::new(
                BedrockCachePlacement::Tools,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
            BedrockCachePoint::new(BedrockCachePlacement::System, BedrockCacheTtl::OneHour),
        ],
        cache_capabilities(
            FOUNDATION_MODEL,
            vec![BedrockCachePlacement::Tools, BedrockCachePlacement::System],
            2,
            CapabilitySupport::Supported,
        ),
    )
    .expect_err("one-hour checkpoints must precede five-minute checkpoints");
    assert_eq!(error.field(), "cache_points");
}

#[test]
fn duplicate_or_empty_cache_checkpoint_sets_are_refused() {
    assert_eq!(
        BedrockPromptCacheConfig::new(
            Vec::new(),
            cache_capabilities(
                FOUNDATION_MODEL,
                vec![BedrockCachePlacement::System],
                1,
                CapabilitySupport::Unsupported,
            ),
        )
        .expect_err("an enabled cache policy must contain a checkpoint")
        .field(),
        "cache_points"
    );
    assert_eq!(
        BedrockPromptCacheConfig::new(
            vec![
                BedrockCachePoint::new(BedrockCachePlacement::System, BedrockCacheTtl::OneHour,),
                BedrockCachePoint::new(
                    BedrockCachePlacement::System,
                    BedrockCacheTtl::DefaultFiveMinutes,
                ),
            ],
            cache_capabilities(
                FOUNDATION_MODEL,
                vec![BedrockCachePlacement::System],
                2,
                CapabilitySupport::Supported,
            ),
        )
        .expect_err("one high-level checkpoint per content plane")
        .field(),
        "cache_points"
    );
}

#[test]
fn cache_configuration_enforces_exact_model_placement_count_and_ttl_evidence() {
    let system_only = cache_capabilities(
        FOUNDATION_MODEL,
        vec![BedrockCachePlacement::System],
        1,
        CapabilitySupport::Unsupported,
    );
    let placement_error = BedrockPromptCacheConfig::new(
        vec![BedrockCachePoint::new(
            BedrockCachePlacement::Tools,
            BedrockCacheTtl::DefaultFiveMinutes,
        )],
        system_only.clone(),
    )
    .expect_err("unsupported placement must fail");
    assert_eq!(
        placement_error.class(),
        BedrockMetadataErrorClass::Unsupported
    );
    assert_eq!(placement_error.field(), "cache_placements");

    let count_error = BedrockPromptCacheConfig::new(
        vec![
            BedrockCachePoint::new(
                BedrockCachePlacement::System,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
            BedrockCachePoint::new(
                BedrockCachePlacement::LatestUserMessage,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
        ],
        system_only.clone(),
    )
    .expect_err("selected model maximum must bind");
    assert_eq!(count_error.class(), BedrockMetadataErrorClass::Unsupported);
    assert_eq!(count_error.field(), "cache_points");

    let ttl_error = BedrockPromptCacheConfig::new(
        vec![BedrockCachePoint::new(
            BedrockCachePlacement::System,
            BedrockCacheTtl::OneHour,
        )],
        system_only,
    )
    .expect_err("unsupported one-hour TTL must fail");
    assert_eq!(ttl_error.class(), BedrockMetadataErrorClass::Unsupported);
    assert_eq!(ttl_error.field(), "cache_ttl");

    let unknown_ttl = BedrockPromptCacheConfig::new(
        vec![BedrockCachePoint::new(
            BedrockCachePlacement::System,
            BedrockCacheTtl::OneHour,
        )],
        cache_capabilities(
            FOUNDATION_MODEL,
            vec![BedrockCachePlacement::System],
            1,
            CapabilitySupport::Unknown,
        ),
    )
    .expect_err("Unknown one-hour evidence must not become Supported");
    assert_eq!(unknown_ttl.class(), BedrockMetadataErrorClass::Unproven);
    assert_eq!(unknown_ttl.field(), "cache_ttl");
}

#[test]
fn cache_capability_evidence_is_bound_to_the_selected_canonical_model() {
    let cache = BedrockPromptCacheConfig::new(
        vec![BedrockCachePoint::new(
            BedrockCachePlacement::System,
            BedrockCacheTtl::DefaultFiveMinutes,
        )],
        cache_capabilities(
            FOUNDATION_MODEL,
            vec![BedrockCachePlacement::System],
            1,
            CapabilitySupport::Unsupported,
        ),
    )
    .unwrap();
    let metadata = BedrockRuntimeRequestMetadata::new(Some(cache), None);
    let error = metadata
        .to_provider_option(
            &region(),
            &descriptor("anthropic.different-model-v1", CapabilitySupport::Supported),
        )
        .expect_err("capability evidence for another model must not cross selection");
    assert_eq!(error.class(), BedrockMetadataErrorClass::Unproven);
    assert_eq!(error.field(), "prompt_cache");
}

#[test]
fn guardrail_metadata_validates_the_documented_identifier_version_and_stream_enums() {
    let guardrail = BedrockGuardrailConfig::new("grabc123", "DRAFT")
        .unwrap()
        .with_trace(BedrockGuardrailTrace::EnabledFull)
        .with_stream_mode(BedrockGuardrailStreamMode::Sync);
    assert_eq!(guardrail.identifier(), "grabc123");
    assert_eq!(guardrail.version(), "DRAFT");
    assert_eq!(guardrail.trace(), Some(BedrockGuardrailTrace::EnabledFull));
    assert_eq!(
        guardrail.stream_mode(),
        Some(BedrockGuardrailStreamMode::Sync)
    );

    let arn = "arn:aws:bedrock:us-east-1:123456789012:guardrail/grabc123";
    assert!(BedrockGuardrailConfig::new(arn, "17").is_ok());
    for bad_version in ["", "0", "01", "draft", "100000000"] {
        assert_eq!(
            BedrockGuardrailConfig::new("grabc123", bad_version)
                .expect_err("invalid guardrail version")
                .field(),
            "guardrail_version"
        );
    }
}

#[test]
fn invalid_guardrail_diagnostics_never_repeat_the_rejected_identifier() {
    let rejected = "credential-shaped-but-not-a-guardrail";
    let error = BedrockGuardrailConfig::new(rejected, "DRAFT")
        .expect_err("hyphens are outside the simple id alphabet");
    assert!(!error.to_string().contains(rejected));
    assert!(!format!("{error:?}").contains(rejected));
}

#[test]
fn route_metadata_distinguishes_foundation_geographic_global_and_application_targets() {
    let foundation = BedrockRouteMetadata::new(&region(), FOUNDATION_MODEL).unwrap();
    assert_eq!(
        foundation.target_kind(),
        BedrockInferenceTargetKind::FoundationModel
    );
    assert_eq!(foundation.cross_region_scope(), None);

    let geographic =
        BedrockRouteMetadata::new(&region(), "us.anthropic.claude-sonnet-4-6-v1:0").unwrap();
    assert_eq!(
        geographic.target_kind(),
        BedrockInferenceTargetKind::CrossRegionInferenceProfile
    );
    assert_eq!(
        geographic.cross_region_scope(),
        Some(BedrockCrossRegionScope::Geographic)
    );

    let global = BedrockRouteMetadata::new(
        &region(),
        "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
    )
    .unwrap();
    assert_eq!(
        global.cross_region_scope(),
        Some(BedrockCrossRegionScope::Global)
    );

    let application = BedrockRouteMetadata::new(
        &region(),
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/my-team-profile",
    )
    .unwrap();
    assert_eq!(
        application.target_kind(),
        BedrockInferenceTargetKind::ApplicationInferenceProfile
    );
    assert_eq!(application.cross_region_scope(), None);
}

#[test]
fn unsafe_or_ambiguous_runtime_targets_are_refused_without_echoing_them() {
    for rejected in [
        "",
        "../secret",
        "anthropic..claude",
        "plain/model",
        "model?query",
        "model#fragment",
        "model with spaces",
        "arn:aws:bedrock:US-EAST-1:123456789012:inference-profile/team",
        "arn:aws:bedrock:us-east-1:not-an-account:inference-profile/team",
        "arn:aws:bedrock:us-east-1:123:application-inference-profile/team",
        "arn:aws:bedrock:us-east-1:123456789012:unknown-resource/team",
    ] {
        let error = BedrockRouteMetadata::new(&region(), rejected)
            .expect_err("unsafe runtime target must fail");
        assert_eq!(error.field(), "model_id");
        if !rejected.is_empty() {
            assert!(!error.to_string().contains(rejected));
            assert!(!format!("{error:?}").contains(rejected));
        }
    }
}

#[test]
fn every_documented_converse_runtime_arn_class_is_admitted_and_classified() {
    let cases = [
        (
            "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-4-6-v1",
            BedrockInferenceTargetKind::FoundationModel,
        ),
        (
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-sonnet-4-6-v1:0",
            BedrockInferenceTargetKind::CrossRegionInferenceProfile,
        ),
        (
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/team-profile",
            BedrockInferenceTargetKind::ApplicationInferenceProfile,
        ),
        (
            "arn:aws:bedrock:us-east-1:123456789012:custom-model-deployment/abc123def456",
            BedrockInferenceTargetKind::Other,
        ),
        (
            "arn:aws:sagemaker:us-east-1:123456789012:endpoint/marketplace-endpoint",
            BedrockInferenceTargetKind::Other,
        ),
    ];
    for (target, expected) in cases {
        assert_eq!(
            BedrockRouteMetadata::new(&region(), target)
                .unwrap()
                .target_kind(),
            expected,
            "{target}"
        );
    }
}

#[test]
fn runtime_metadata_becomes_one_redacted_provider_option_with_request_facts() {
    let selected_id = "global.anthropic.claude-sonnet-4-5-20250929-v1:0";
    let cache = BedrockPromptCacheConfig::new(
        vec![
            BedrockCachePoint::new(BedrockCachePlacement::Tools, BedrockCacheTtl::OneHour),
            BedrockCachePoint::new(
                BedrockCachePlacement::System,
                BedrockCacheTtl::DefaultFiveMinutes,
            ),
        ],
        cache_capabilities(
            selected_id,
            vec![BedrockCachePlacement::Tools, BedrockCachePlacement::System],
            2,
            CapabilitySupport::Supported,
        ),
    )
    .unwrap();
    let guardrail = BedrockGuardrailConfig::new("grabc123", "7")
        .unwrap()
        .with_trace(BedrockGuardrailTrace::Enabled)
        .with_stream_mode(BedrockGuardrailStreamMode::Async);
    let metadata = BedrockRuntimeRequestMetadata::new(Some(cache), Some(guardrail));
    let selected = descriptor(selected_id, CapabilitySupport::Supported);
    let option = metadata.to_provider_option(&region(), &selected).unwrap();
    assert_eq!(option.provider(), BEDROCK_PROVIDER);
    assert_eq!(option.kind(), "runtime-metadata");
    assert_eq!(
        option.data(),
        &serde_json::json!({
            "route": {
                "source_region": TEST_REGION,
                "target_kind": "cross_region_inference_profile",
                "cross_region_scope": "global"
            },
            "cache_points": [
                {"placement":"tools","cachePoint":{"type":"default","ttl":"1h"}},
                {"placement":"system","cachePoint":{"type":"default"}}
            ],
            "guardrailConfig": {
                "guardrailIdentifier":"grabc123",
                "guardrailVersion":"7",
                "trace":"enabled",
                "streamProcessingMode":"async"
            }
        })
    );
    assert!(!format!("{option:?}").contains("grabc123"));
}

#[test]
fn shared_converse_adapter_accepts_selected_model_runtime_metadata_before_transport() {
    let metadata = BedrockRuntimeRequestMetadata::new(
        Some(
            BedrockPromptCacheConfig::new(
                vec![BedrockCachePoint::new(
                    BedrockCachePlacement::System,
                    BedrockCacheTtl::DefaultFiveMinutes,
                )],
                cache_capabilities(
                    FOUNDATION_MODEL,
                    vec![BedrockCachePlacement::System],
                    1,
                    CapabilitySupport::Unsupported,
                ),
            )
            .unwrap(),
        ),
        None,
    );
    let descriptor = descriptor(FOUNDATION_MODEL, CapabilitySupport::Supported);
    let option = metadata.to_provider_option(&region(), &descriptor).unwrap();
    let (http, requests) = http(Vec::new());
    let provider = BedrockConverseProvider::new(
        http,
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        FOUNDATION_MODEL,
    )
    .unwrap()
    .with_runtime_metadata(metadata);
    let mut draft = RequestDraft {
        provider: BEDROCK_PROVIDER.to_owned(),
        model: FOUNDATION_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_000),
        effective_at_ms: 2_000,
        system: Some("stable instructions".to_owned()),
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
    };
    let options = provider
        .request_options_for(ProviderOptionContext::new(&descriptor, &[]))
        .unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0], option);
    draft.provider_options = options.clone();
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(draft, &descriptor)
        .expect("shared Converse metadata dialect");
    assert_eq!(
        call.protocol(),
        heycode_core::ProviderProtocol::BedrockConverse
    );
    assert_eq!(call.provider_options(), options);
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn provider_options_follow_the_selected_model_instead_of_the_provider_default() {
    let metadata = BedrockRuntimeRequestMetadata::new(None, None);
    let (http, _) = http(Vec::new());
    let provider = BedrockConverseProvider::new(
        http,
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        FOUNDATION_MODEL,
    )
    .unwrap()
    .with_runtime_metadata(metadata);
    let options = provider
        .request_options_for(ProviderOptionContext::new(
            &descriptor(
                "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
                CapabilitySupport::Unknown,
            ),
            &[],
        ))
        .unwrap();
    assert_eq!(
        options[0].data()["route"]["target_kind"],
        serde_json::json!("cross_region_inference_profile")
    );
    assert_eq!(
        options[0].data()["route"]["cross_region_scope"],
        serde_json::json!("global")
    );
}

#[test]
fn request_specific_hook_rejects_unimplemented_provider_native_routes() {
    let (http, _) = http(Vec::new());
    let provider = BedrockConverseProvider::new(
        http,
        &region(),
        &CredentialSecret::new(TEST_SECRET),
        FOUNDATION_MODEL,
    )
    .unwrap();
    let selected = descriptor(FOUNDATION_MODEL, CapabilitySupport::Unknown);
    let route = heycode_core::NativeToolRoute::new(
        "web_search",
        "bedrock:web_search",
        heycode_core::NativeToolImplementationKind::Provider,
        Some(BEDROCK_PROVIDER.to_owned()),
    )
    .unwrap();
    let error = provider
        .request_options_for(ProviderOptionContext::new(&selected, &[route]))
        .unwrap_err();
    assert!(matches!(
        error,
        ResolveError::InvalidRequest {
            field: "native_tool_routes",
            ..
        }
    ));
}

#[test]
fn explicit_cache_metadata_requires_affirmative_selected_model_evidence() {
    let metadata = BedrockRuntimeRequestMetadata::new(
        Some(
            BedrockPromptCacheConfig::new(
                vec![BedrockCachePoint::new(
                    BedrockCachePlacement::System,
                    BedrockCacheTtl::DefaultFiveMinutes,
                )],
                cache_capabilities(
                    FOUNDATION_MODEL,
                    vec![BedrockCachePlacement::System],
                    1,
                    CapabilitySupport::Unsupported,
                ),
            )
            .unwrap(),
        ),
        None,
    );
    for (support, class, expected) in [
        (
            CapabilitySupport::Unknown,
            BedrockMetadataErrorClass::Unproven,
            "unproven",
        ),
        (
            CapabilitySupport::Unsupported,
            BedrockMetadataErrorClass::Unsupported,
            "does not support",
        ),
    ] {
        let error = metadata
            .to_provider_option(&region(), &descriptor(FOUNDATION_MODEL, support))
            .expect_err("cache policy needs exact model capability evidence");
        assert_eq!(error.field(), "prompt_cache");
        assert_eq!(error.class(), class);
        assert!(error.to_string().contains(expected), "{error}");
    }
    assert!(
        metadata
            .to_provider_option(
                &region(),
                &descriptor(FOUNDATION_MODEL, CapabilitySupport::Supported),
            )
            .is_ok()
    );
}

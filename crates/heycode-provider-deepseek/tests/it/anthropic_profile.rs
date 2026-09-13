//! DeepSeek's published Anthropic-format compatibility, transcribed and
//! pinned.
//!
//! Every claim here is checked against
//! <https://api-docs.deepseek.com/guides/anthropic_api>,
//! <https://api-docs.deepseek.com/guides/thinking_mode> and
//! <https://api-docs.deepseek.com/quick_start/pricing>. Nothing here calls the
//! live service.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::ProviderProtocol;
use heycode_credentials::{CredentialResolutionError, CredentialSecret};
use heycode_llm::{
    AnthropicAuthWire, AuthenticationBinding, CapabilitySupport, CredentialHandle,
    CredentialResolver, RouteCredential,
};
use heycode_provider_deepseek::{
    DEEPSEEK_ANTHROPIC_BASE_URL, DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL, DEEPSEEK_API_KEY_REFERENCE,
    DEEPSEEK_IGNORED_THINKING_BUDGET_TOKENS, DEEPSEEK_V4_FLASH, DEEPSEEK_V4_FLASH_VISION_EXP,
    DEEPSEEK_V4_MAX_OUTPUT_TOKENS, DEEPSEEK_V4_PRO, DEEPSEEK_V4_PRO_1M, DeepSeekAnthropicEffort,
    DeepSeekAnthropicError, DeepSeekAnthropicField, DeepSeekAnthropicModelRoute,
    DeepSeekAnthropicProfile, DeepSeekAnthropicSupport, admit_model, classify_model,
    temperature_with_thinking_enabled,
};

// ---------------------------------------------------------------------------
// The compatibility table
// ---------------------------------------------------------------------------

#[test]
fn the_signature_deepseek_never_documents_is_unknown_while_the_thinking_block_it_sits_in_is_supported()
 {
    // This pair is the whole point of the row. DeepSeek marks the `thinking`
    // block Supported and lists no sub-fields for it, while the text,
    // tool_use and tool_result rows each get explicit sub-field rows. The
    // block being documented says nothing about the signature inside it.
    assert_eq!(
        DeepSeekAnthropicField::ThinkingBlock.documented_support(),
        DeepSeekAnthropicSupport::Supported
    );
    assert_eq!(
        DeepSeekAnthropicField::ThinkingBlockSignature.documented_support(),
        DeepSeekAnthropicSupport::Undocumented
    );
    assert_eq!(
        DeepSeekAnthropicField::ThinkingBlockSignature.capability(),
        CapabilitySupport::Unknown
    );
}

#[test]
fn every_unknown_field_has_an_explicit_undocumented_or_inferred_source() {
    let unknown: Vec<_> = DeepSeekAnthropicField::ALL
        .into_iter()
        .filter(|field| field.capability() == CapabilitySupport::Unknown)
        .collect();
    for field in &unknown {
        assert!(
            matches!(
                field.documented_support(),
                DeepSeekAnthropicSupport::IntegrationInferred
                    | DeepSeekAnthropicSupport::Undocumented
            ),
            "{field:?} reports Unknown without an Unknown evidence class"
        );
    }
    // Named exhaustively so that adding a field without deciding its evidence
    // fails here rather than defaulting into a claim.
    assert_eq!(
        unknown,
        vec![
            DeepSeekAnthropicField::AuthorizationBearerHeader,
            DeepSeekAnthropicField::WebSearchServerToolDefinition,
            DeepSeekAnthropicField::ImageFileSource,
            DeepSeekAnthropicField::ThinkingBlockSignature,
            DeepSeekAnthropicField::MessagesPath,
            DeepSeekAnthropicField::ResponseStopReason,
            DeepSeekAnthropicField::ResponseUsage,
            DeepSeekAnthropicField::ResponseUsageCacheCounters,
            DeepSeekAnthropicField::StreamingEventNames,
            DeepSeekAnthropicField::ErrorEnvelope,
        ]
    );
}

#[test]
fn response_side_fields_have_no_published_row_and_stay_unknown() {
    // The compatibility page documents headers, request fields, tools and
    // message blocks. It has no response table at all.
    for field in [
        DeepSeekAnthropicField::ResponseStopReason,
        DeepSeekAnthropicField::ResponseUsage,
        DeepSeekAnthropicField::ResponseUsageCacheCounters,
        DeepSeekAnthropicField::StreamingEventNames,
        DeepSeekAnthropicField::ErrorEnvelope,
    ] {
        assert_eq!(field.capability(), CapabilitySupport::Unknown, "{field:?}");
    }
}

#[test]
fn an_ignored_field_is_reported_unsupported_because_accepting_is_not_honouring() {
    for field in [
        DeepSeekAnthropicField::AnthropicBetaHeader,
        DeepSeekAnthropicField::AnthropicVersionHeader,
        DeepSeekAnthropicField::Container,
        DeepSeekAnthropicField::McpServers,
        DeepSeekAnthropicField::MetadataOtherFields,
        DeepSeekAnthropicField::ServiceTier,
        DeepSeekAnthropicField::ThinkingBudgetTokens,
        DeepSeekAnthropicField::OutputConfigOtherFields,
        DeepSeekAnthropicField::TopK,
        DeepSeekAnthropicField::ToolCacheControl,
        DeepSeekAnthropicField::DisableParallelToolUse,
        DeepSeekAnthropicField::TextCacheControl,
        DeepSeekAnthropicField::TextCitations,
        DeepSeekAnthropicField::ToolUseCacheControl,
        DeepSeekAnthropicField::ToolResultCacheControl,
        DeepSeekAnthropicField::ToolResultIsError,
    ] {
        assert_eq!(
            field.documented_support(),
            DeepSeekAnthropicSupport::Ignored,
            "{field:?}"
        );
        assert_eq!(
            field.capability(),
            CapabilitySupport::Unsupported,
            "{field:?}"
        );
    }
}

#[test]
fn every_cache_control_row_is_ignored_so_prompt_caching_is_unaddressable_here() {
    for field in [
        DeepSeekAnthropicField::ToolCacheControl,
        DeepSeekAnthropicField::TextCacheControl,
        DeepSeekAnthropicField::ToolUseCacheControl,
        DeepSeekAnthropicField::ToolResultCacheControl,
    ] {
        assert_eq!(
            field.documented_support(),
            DeepSeekAnthropicSupport::Ignored,
            "{field:?}"
        );
    }
}

#[test]
fn the_blocks_deepseek_refuses_are_not_supported_rather_than_merely_ignored() {
    for field in [
        DeepSeekAnthropicField::DocumentBlock,
        DeepSeekAnthropicField::SearchResultBlock,
        DeepSeekAnthropicField::RedactedThinkingBlock,
        DeepSeekAnthropicField::CodeExecutionToolResultBlock,
        DeepSeekAnthropicField::McpToolUseBlock,
        DeepSeekAnthropicField::McpToolResultBlock,
        DeepSeekAnthropicField::ContainerUploadBlock,
    ] {
        assert_eq!(
            field.documented_support(),
            DeepSeekAnthropicSupport::NotSupported,
            "{field:?}"
        );
    }
}

#[test]
fn the_model_field_is_remapped_rather_than_supported_or_refused() {
    assert_eq!(
        DeepSeekAnthropicField::Model.documented_support(),
        DeepSeekAnthropicSupport::Remapped
    );
    assert_eq!(
        DeepSeekAnthropicField::Model.capability(),
        CapabilitySupport::Unsupported
    );
}

#[test]
fn the_web_search_response_blocks_are_supported_while_its_request_definition_is_not_published() {
    assert_eq!(
        DeepSeekAnthropicField::ServerToolUseBlock.documented_support(),
        DeepSeekAnthropicSupport::Supported
    );
    assert_eq!(
        DeepSeekAnthropicField::WebSearchToolResultBlock.documented_support(),
        DeepSeekAnthropicSupport::Supported
    );
    assert_eq!(
        DeepSeekAnthropicField::WebSearchServerToolDefinition.capability(),
        CapabilitySupport::Unknown
    );
}

#[test]
fn the_file_image_source_contradicts_the_ignored_anthropic_beta_header_so_it_stays_unknown() {
    // The image row requires `anthropic-beta: files-api-2025-04-14` for the
    // `file` variant; the header row says `anthropic-beta` is ignored for
    // `/messages`. Both cannot hold for the same request.
    assert_eq!(
        DeepSeekAnthropicField::ImageBlock.documented_support(),
        DeepSeekAnthropicSupport::Supported
    );
    assert_eq!(
        DeepSeekAnthropicField::AnthropicBetaHeader.documented_support(),
        DeepSeekAnthropicSupport::Ignored
    );
    assert_eq!(
        DeepSeekAnthropicField::ImageFileSource.capability(),
        CapabilitySupport::Unknown
    );
}

#[test]
fn temperature_is_tabled_supported_but_its_thinking_mode_interaction_stays_unknown() {
    assert_eq!(
        DeepSeekAnthropicField::Temperature.documented_support(),
        DeepSeekAnthropicSupport::Supported
    );
    assert_eq!(
        temperature_with_thinking_enabled(),
        CapabilitySupport::Unknown
    );
}

#[test]
fn x_api_key_is_documented_while_bearer_auth_is_only_inferred_from_the_claude_code_recipe() {
    assert_eq!(
        DeepSeekAnthropicField::XApiKeyHeader.documented_support(),
        DeepSeekAnthropicSupport::Supported
    );
    assert_eq!(
        DeepSeekAnthropicField::AuthorizationBearerHeader.documented_support(),
        DeepSeekAnthropicSupport::IntegrationInferred
    );
    assert_eq!(
        DeepSeekAnthropicField::AuthorizationBearerHeader.capability(),
        CapabilitySupport::Unknown
    );
}

#[test]
fn the_field_list_covers_every_variant_it_declares_exactly_once() {
    let mut seen = DeepSeekAnthropicField::ALL.to_vec();
    let declared = seen.len();
    assert_eq!(declared, 62, "the exhaustive field registry drifted");
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), declared);
}

#[test]
fn support_states_render_stable_lowercase_names() {
    assert_eq!(DeepSeekAnthropicSupport::Supported.as_str(), "supported");
    assert_eq!(DeepSeekAnthropicSupport::Ignored.as_str(), "ignored");
    assert_eq!(
        DeepSeekAnthropicSupport::NotSupported.as_str(),
        "not-supported"
    );
    assert_eq!(DeepSeekAnthropicSupport::Remapped.as_str(), "remapped");
    assert_eq!(
        DeepSeekAnthropicSupport::IntegrationInferred.as_str(),
        "integration-inferred"
    );
    assert_eq!(
        DeepSeekAnthropicSupport::Undocumented.as_str(),
        "undocumented"
    );
}

// ---------------------------------------------------------------------------
// Model routing
// ---------------------------------------------------------------------------

#[test]
fn every_model_deepseek_publishes_for_this_route_classifies_as_published() {
    for model in [
        DEEPSEEK_V4_FLASH,
        DEEPSEEK_V4_PRO,
        DEEPSEEK_V4_FLASH_VISION_EXP,
    ] {
        assert_eq!(
            classify_model(model),
            DeepSeekAnthropicModelRoute::Published { model }
        );
        assert_eq!(admit_model(model).unwrap(), model);
    }
}

#[test]
fn the_claude_code_only_pro_id_classifies_as_integration_guide_only() {
    // `deepseek-v4-pro[1m]` is what DeepSeek's own Claude Code guide sets as
    // ANTHROPIC_MODEL, yet the model table does not list it. It is admitted
    // because DeepSeek instructs sending it, and marked apart because nothing
    // published says what it selects.
    assert_eq!(
        classify_model(DEEPSEEK_V4_PRO_1M),
        DeepSeekAnthropicModelRoute::IntegrationGuideOnly {
            model: DEEPSEEK_V4_PRO_1M
        }
    );
    assert_eq!(admit_model(DEEPSEEK_V4_PRO_1M).unwrap(), DEEPSEEK_V4_PRO_1M);
}

#[test]
fn claude_opus_names_map_to_pro_and_haiku_or_sonnet_names_map_to_flash() {
    assert_eq!(
        classify_model("claude-opus-5"),
        DeepSeekAnthropicModelRoute::ClaudeAlias {
            prefix: "claude-opus",
            model: DEEPSEEK_V4_PRO
        }
    );
    for requested in ["claude-haiku-4-5", "claude-sonnet-4-5"] {
        let route = classify_model(requested);
        assert_eq!(route.model(), DEEPSEEK_V4_FLASH, "{requested}");
        assert!(
            matches!(route, DeepSeekAnthropicModelRoute::ClaudeAlias { .. }),
            "{requested} classified as {route:?}"
        );
    }
}

#[test]
fn the_bare_prefixes_themselves_map_without_a_version_suffix() {
    assert_eq!(classify_model("claude-opus").model(), DEEPSEEK_V4_PRO);
    assert_eq!(classify_model("claude-haiku").model(), DEEPSEEK_V4_FLASH);
    assert_eq!(classify_model("claude-sonnet").model(), DEEPSEEK_V4_FLASH);
}

#[test]
fn an_unrecognized_model_is_refused_because_deepseek_substitutes_flash_without_saying_so() {
    for requested in [
        "deepseek-chat",
        "deepseek-reasoner",
        "deepseek-v4-prro",
        "gpt-5",
        "",
        // Case matters: DeepSeek documents lowercase prefixes only, so an
        // uppercase name is refused rather than assumed to match.
        "Claude-Opus-5",
    ] {
        let route = classify_model(requested);
        assert!(
            route.is_silent_fallback(),
            "{requested} classified as {route:?}"
        );
        assert_eq!(route.model(), DEEPSEEK_V4_FLASH);
        assert_eq!(
            admit_model(requested),
            Err(DeepSeekAnthropicError::SilentModelFallback {
                requested: requested.to_owned(),
                served: DEEPSEEK_V4_FLASH,
            })
        );
    }
}

#[test]
fn the_silent_fallback_error_names_both_the_request_and_the_substitute() {
    let message = admit_model("deepseek-chat").unwrap_err().to_string();
    assert!(message.contains("deepseek-chat"), "{message}");
    assert!(message.contains(DEEPSEEK_V4_FLASH), "{message}");
}

#[test]
fn only_the_unmapped_route_reports_a_silent_fallback() {
    assert!(!classify_model(DEEPSEEK_V4_PRO).is_silent_fallback());
    assert!(!classify_model(DEEPSEEK_V4_PRO_1M).is_silent_fallback());
    assert!(!classify_model("claude-opus-5").is_silent_fallback());
    assert!(classify_model("nonsense").is_silent_fallback());
}

// ---------------------------------------------------------------------------
// Reasoning choices
// ---------------------------------------------------------------------------

#[test]
fn the_offered_efforts_are_exactly_deepseeks_anthropic_column_plus_the_disabled_toggle() {
    let ids: Vec<_> = DeepSeekAnthropicEffort::ALL
        .into_iter()
        .map(DeepSeekAnthropicEffort::canonical_id)
        .collect();
    assert_eq!(ids, vec!["none", "low", "high", "max"]);
    let wire: Vec<_> = DeepSeekAnthropicEffort::ALL
        .into_iter()
        .map(DeepSeekAnthropicEffort::wire_effort)
        .collect();
    assert_eq!(wire, vec![None, Some("low"), Some("high"), Some("max")]);
}

#[test]
fn the_default_effort_is_the_high_deepseek_documents() {
    assert_eq!(
        DeepSeekAnthropicEffort::DEFAULT,
        DeepSeekAnthropicEffort::High
    );
    assert_eq!(DeepSeekAnthropicEffort::DEFAULT.wire_effort(), Some("high"));
}

#[test]
fn the_remapped_medium_and_xhigh_values_are_not_offered_as_distinct_choices() {
    // DeepSeek accepts both and silently answers with `high`. Offering them
    // would mean a user choice quietly becoming a different choice.
    let ids: Vec<_> = DeepSeekAnthropicEffort::ALL
        .into_iter()
        .map(DeepSeekAnthropicEffort::canonical_id)
        .collect();
    assert!(!ids.contains(&"medium"));
    assert!(!ids.contains(&"xhigh"));
}

// ---------------------------------------------------------------------------
// The profile
// ---------------------------------------------------------------------------

#[test]
fn the_route_base_url_appends_the_anthropic_sdk_v1_segment_to_deepseeks_published_base() {
    assert_eq!(
        DEEPSEEK_ANTHROPIC_BASE_URL,
        "https://api.deepseek.com/anthropic"
    );
    assert_eq!(
        DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL,
        format!("{DEEPSEEK_ANTHROPIC_BASE_URL}/v1")
    );
    assert_eq!(
        DeepSeekAnthropicProfile::api_key().base_url(),
        DEEPSEEK_ANTHROPIC_MESSAGES_BASE_URL
    );
}

#[test]
fn the_two_profiles_differ_only_in_the_auth_header_they_send() {
    let key = DeepSeekAnthropicProfile::api_key();
    let token = DeepSeekAnthropicProfile::auth_token();
    assert_eq!(key.auth_wire(), AnthropicAuthWire::XApiKey);
    assert_eq!(token.auth_wire(), AnthropicAuthWire::Bearer);
    assert_eq!(key.base_url(), token.base_url());
    assert_eq!(key.max_output_tokens(), token.max_output_tokens());
}

#[test]
fn the_default_profile_is_the_x_api_key_one_the_table_documents() {
    assert_eq!(
        DeepSeekAnthropicProfile::default(),
        DeepSeekAnthropicProfile::api_key()
    );
}

#[test]
fn the_output_default_is_deepseeks_published_maximum_rather_than_an_invented_number() {
    assert_eq!(DEEPSEEK_V4_MAX_OUTPUT_TOKENS, 393_216);
    assert_eq!(
        DeepSeekAnthropicProfile::api_key().max_output_tokens(),
        DEEPSEEK_V4_MAX_OUTPUT_TOKENS
    );
    assert_eq!(
        DeepSeekAnthropicProfile::api_key()
            .with_max_output_tokens(8_192)
            .max_output_tokens(),
        8_192
    );
}

/// Both operands are constants, so this is a compile-time guarantee rather
/// than a test that runs — a `const` assertion cannot be skipped, filtered out
/// or left un-run, which is strictly stronger than the runtime form clippy
/// flagged as constant.
const _: () = assert!(DEEPSEEK_IGNORED_THINKING_BUDGET_TOKENS < DEEPSEEK_V4_MAX_OUTPUT_TOKENS);

#[test]
fn the_ignored_thinking_budget_is_the_adapters_own_minimum() {
    // DeepSeek ignores `budget_tokens`, so no value here means anything. The
    // adapter's floor is sent so that nothing reads as a chosen budget.
    assert_eq!(DEEPSEEK_IGNORED_THINKING_BUDGET_TOKENS, 1_024);
}

#[test]
fn the_provider_descriptor_declares_both_dialects_deepseek_publishes() {
    let descriptor = DeepSeekAnthropicProfile::api_key().provider_descriptor();
    assert_eq!(descriptor.id, "deepseek");
    assert!(
        descriptor
            .protocols
            .contains(&ProviderProtocol::AnthropicMessages)
    );
    assert!(
        descriptor
            .protocols
            .contains(&ProviderProtocol::OpenAiChatCompletions)
    );
}

#[test]
fn the_provider_profile_names_the_credential_reference_deepseek_documents() {
    let profile = DeepSeekAnthropicProfile::api_key().provider_profile();
    assert_eq!(profile.registry_name, "deepseek");
    assert_eq!(profile.default_model, DEEPSEEK_V4_FLASH);
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(DEEPSEEK_API_KEY_REFERENCE)
    );
}

#[test]
fn the_messages_config_resolve_spec_offers_every_documented_effort_and_defaults_to_high() {
    let config = DeepSeekAnthropicProfile::api_key()
        .messages_config("test-key")
        .unwrap();
    let spec = config.resolve_spec();
    assert_eq!(spec.protocol, ProviderProtocol::AnthropicMessages);
    let efforts: Vec<_> = spec
        .reasoning_efforts
        .iter()
        .map(|effort| effort.as_str().to_owned())
        .collect();
    assert_eq!(efforts, vec!["none", "low", "high", "max"]);
    assert_eq!(
        spec.default_reasoning_effort
            .map(|id| id.as_str().to_owned()),
        Some("high".to_owned())
    );
    assert_eq!(
        spec.default_max_output_tokens,
        Some(DEEPSEEK_V4_MAX_OUTPUT_TOKENS)
    );
}

struct NeverResolvingCredential {
    route: CredentialHandle,
}

impl CredentialResolver for NeverResolvingCredential {
    fn route(&self) -> &CredentialHandle {
        &self.route
    }

    fn resolve(
        &self,
        _route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        panic!("resolve_spec must not acquire a credential")
    }
}

#[test]
fn the_production_config_retains_a_non_secret_operation_time_credential_binding() {
    let route = CredentialHandle::new(DEEPSEEK_API_KEY_REFERENCE).unwrap();
    let credential = RouteCredential::per_operation(
        route.clone(),
        Arc::new(NeverResolvingCredential {
            route: route.clone(),
        }),
    );
    let config = DeepSeekAnthropicProfile::api_key()
        .messages_config_with_credential(credential)
        .unwrap();
    assert_eq!(
        config.resolve_spec().authentication,
        AuthenticationBinding::Credential(route)
    );
}

#[test]
fn the_messages_config_never_prints_its_key() {
    let config = DeepSeekAnthropicProfile::api_key()
        .messages_config("super-secret-deepseek-key")
        .unwrap();
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("super-secret-deepseek-key"),
        "{rendered}"
    );
    assert!(rendered.contains("REDACTED"), "{rendered}");
}
